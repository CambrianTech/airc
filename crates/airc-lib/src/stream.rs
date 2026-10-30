use std::collections::{BTreeSet, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use airc_core::transcript::TranscriptKind;
use airc_core::{
    ClientId, HeaderFilter, PeerId, RoomId, SelfFilter, TranscriptCursor, TranscriptEvent,
};
use futures::stream::Stream;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, ReceiverStream};

/// Live transcript-event stream returned by `Airc::subscribe`.
///
/// Two sources, one interface: the in-process **broadcast** fan-out
/// (embedded SDK + cross-machine transports) and the **daemon** path
/// (one IPC attach per subscribed channel, merged into a single
/// receiver). The owner-core router subscribes per channel, so the
/// daemon variant fans N attach streams into one ordered-per-channel
/// feed — the same merge the CLI monitor does, lifted into the SDK.
pub struct EventStream {
    inner: EventStreamInner,
}

enum EventStreamInner {
    Closed,
    Broadcast(BroadcastStream<Arc<TranscriptEvent>>),
    Daemon {
        rx: ReceiverStream<Result<Arc<TranscriptEvent>, StreamError>>,
        /// Aborts the per-channel attach tasks when the stream drops, so
        /// closing a subscription tears down its IPC connections.
        _guard: DaemonAttachGuard,
    },
}

impl EventStream {
    pub(crate) fn from_broadcast(rx: broadcast::Receiver<Arc<TranscriptEvent>>) -> Self {
        Self {
            inner: EventStreamInner::Broadcast(BroadcastStream::new(rx)),
        }
    }

    pub(crate) fn daemon(
        rx: mpsc::Receiver<Result<Arc<TranscriptEvent>, StreamError>>,
        handles: Vec<JoinHandle<()>>,
    ) -> Self {
        Self {
            inner: EventStreamInner::Daemon {
                rx: ReceiverStream::new(rx),
                _guard: DaemonAttachGuard { handles },
            },
        }
    }
}

/// Owns the spawned per-channel attach tasks; aborting on drop keeps the
/// IPC connections tied to the `EventStream`'s lifetime (no detached
/// background work).
struct DaemonAttachGuard {
    handles: Vec<JoinHandle<()>>,
}

impl Drop for DaemonAttachGuard {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

impl Stream for EventStream {
    type Item = Result<Arc<TranscriptEvent>, StreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match &mut this.inner {
            EventStreamInner::Closed => Poll::Ready(None),
            EventStreamInner::Broadcast(stream) => match Pin::new(stream).poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(event))),
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(n)))) => {
                    Poll::Ready(Some(Err(StreamError::Lag(LiveLag { skipped: n }))))
                }
                Poll::Ready(None) => Poll::Ready(None),
            },
            EventStreamInner::Daemon { rx, .. } => match Pin::new(rx).poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Some(Err(error))) => {
                    // Terminate sibling readers with the same subscription on refusal.
                    this.inner = EventStreamInner::Closed;
                    Poll::Ready(Some(Err(error)))
                }
                Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(event))),
                Poll::Ready(None) => Poll::Ready(None),
            },
        }
    }
}

/// Delivery failures are distinct from recoverable broadcast lag.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    #[error(transparent)]
    Lag(#[from] LiveLag),
    #[error("event owner rejected delivery: {message}")]
    OwnerRejected { message: String },
    #[error("event stream decode failed: {message}")]
    Decode { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLag {
    pub skipped: u64,
}

/// One bounded transcript scan, with progress independent of display filters.
/// An empty `events` with `scanned_through: Some(_)` is a filtered page, not
/// exhaustion. Resume from that cursor to reach later matching events.
pub struct EventScan {
    pub events: Vec<TranscriptEvent>,
    pub scanned_through: Option<TranscriptCursor>,
}

impl std::fmt::Display for LiveLag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "live stream lagged {} events; resume from cursor",
            self.skipped
        )
    }
}

impl std::error::Error for LiveLag {}

/// Consumer-facing filter over persisted transcript events.
///
/// This intentionally mirrors the wire-level subscription shape but
/// operates on `TranscriptEvent`, which is what hooks, monitors, and
/// Continuum consume after signatures have been verified and frames
/// have crossed into the durable store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    pub sender: Option<PeerId>,
    pub selection: airc_core::EventSelection,
    pub channel: Option<RoomId>,
    pub channels: HashSet<RoomId>,
    pub kinds: BTreeSet<TranscriptKind>,
    pub headers_filter: HeaderFilter,
    /// DISPLAY-only suppression of the receiver's own broadcasts. `None`
    /// (the default) shows everything — so RAG, Continuum, and any other
    /// consumer that does not opt in is completely unaffected. Only a live
    /// display feed (e.g. `airc join`) sets this, and only to stop
    /// rendering its own sends back at itself. Suppression is purely a
    /// view concern: events are durably stored BEFORE any filter runs, and
    /// `page_recent` (the persona-RAG path) applies no `EventFilter` at
    /// all, so a persona always retains its own turns in recall.
    pub self_echo: Option<SelfEcho>,
}

/// Receiver identity + policy for hiding the receiver's own events from a
/// display feed. See [`EventFilter::self_echo`] — display-only, never a
/// persistence or RAG concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfEcho {
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub filter: SelfFilter,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            sender: None,
            selection: airc_core::EventSelection::All,
            channel: None,
            channels: HashSet::new(),
            kinds: BTreeSet::new(),
            headers_filter: HeaderFilter::Any,
            self_echo: None,
        }
    }
}

impl EventFilter {
    pub fn current_room() -> Self {
        Self::default()
    }

    /// Opt this (display) feed into suppressing the receiver's own
    /// broadcasts. Use [`SelfFilter::ExcludeSamePeer`] for an agent's own
    /// live feed (hide all of my sends regardless of which CLI process
    /// emitted them) or [`SelfFilter::ExcludeSameClient`] to keep cross-tab
    /// visibility. Builder so existing call sites are untouched.
    pub fn excluding_self_echo(
        mut self,
        peer_id: PeerId,
        client_id: ClientId,
        filter: SelfFilter,
    ) -> Self {
        self.self_echo = Some(SelfEcho {
            peer_id,
            client_id,
            filter,
        });
        self
    }

    pub fn matches(&self, event: &TranscriptEvent) -> bool {
        if self.sender.is_some_and(|peer| peer != event.peer_id) {
            return false;
        }
        if !self.selection.matches(event.class) {
            return false;
        }
        if let Some(channel) = self.channel {
            if event.room_id != channel {
                return false;
            }
        }
        if !self.channels.is_empty() && !self.channels.contains(&event.room_id) {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&event.class.transcript_kind()) {
            return false;
        }
        if let Some(se) = self.self_echo {
            if event.is_self_echo(&se.peer_id, &se.client_id, se.filter) {
                return false;
            }
        }
        self.headers_filter.matches(&event.headers)
    }
}

pub struct FilteredEventStream {
    pub(crate) inner: EventStream,
    pub(crate) filter: EventFilter,
}

impl Stream for FilteredEventStream {
    type Item = Result<Arc<TranscriptEvent>, StreamError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(event))) => {
                    if this.filter.matches(event.as_ref()) {
                        return Poll::Ready(Some(Ok(event)));
                    }
                }
                Poll::Ready(Some(Err(lag))) => return Poll::Ready(Some(Err(lag))),
                Poll::Ready(None) => return Poll::Ready(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use airc_core::{Body, ClientId, EventId, Headers, MentionTarget, PeerId};

    use super::*;

    fn event(room_id: RoomId) -> TranscriptEvent {
        TranscriptEvent {
            class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            event_id: EventId::new(),
            room_id,
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: 1,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text("test")),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn semantic_selection_is_independent_of_body_and_labels() {
        let filter = EventFilter {
            selection: airc_core::EventSelection::Correspondence,
            ..EventFilter::default()
        };
        let mut value = event(RoomId::new());
        for body in [
            Body::text("Codex: @Fable 🛠️ {\"kind\":\"alive\"}"),
            Body::Binary(vec![255, 0, 7]),
        ] {
            value.body = Some(body);
            value.class =
                airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message);
            assert!(filter.matches(&value));
            value.class = airc_core::EventClass::Activity(airc_core::ActivityKind::WorkReceipt);
            assert!(!filter.matches(&value));
        }
    }

    fn event_from(room_id: RoomId, peer: PeerId, client: ClientId) -> TranscriptEvent {
        let mut ev = event(room_id);
        ev.peer_id = peer;
        ev.client_id = client;
        ev
    }

    #[test]
    fn event_filter_uses_channel_membership_set() {
        let admitted = RoomId::from_u128(0x01);
        let other = RoomId::from_u128(0x02);
        let filter = EventFilter {
            channels: HashSet::from([admitted]),
            ..EventFilter::default()
        };

        assert!(filter.matches(&event(admitted)));
        assert!(!filter.matches(&event(other)));
    }

    // what this catches: the DISPLAY-only self-echo suppression. An agent's
    // `airc join` feed must not render its own broadcasts back at it (the
    // Monitor-echo noise), but must still show peers — and the default filter
    // (no self_echo) must keep showing everything so RAG/Continuum are
    // unaffected. Regression here = either the agent sees its own sends again
    // or a default consumer silently loses events.
    #[test]
    fn self_echo_filter_hides_own_peer_only_when_opted_in() {
        let room = RoomId::from_u128(0x10);
        let me = PeerId::from_u128(0xaaaa);
        let my_join_client = ClientId::from_u128(0x1);
        let my_msg_client = ClientId::from_u128(0x2); // separate `airc msg` process
        let peer = PeerId::from_u128(0xbbbb);
        let peer_client = ClientId::from_u128(0x3);

        let my_send = event_from(room, me, my_msg_client);
        let peer_send = event_from(room, peer, peer_client);

        // Default filter: shows everything, including our own send (so RAG and
        // any non-opted-in consumer is untouched).
        let default = EventFilter::default();
        assert!(default.matches(&my_send));
        assert!(default.matches(&peer_send));

        // Agent feed: ExcludeSamePeer hides our own send even though it came
        // from a DIFFERENT client id than the join feed, and still shows peers.
        let agent_feed = EventFilter::default().excluding_self_echo(
            me,
            my_join_client,
            SelfFilter::ExcludeSamePeer,
        );
        assert!(
            !agent_feed.matches(&my_send),
            "own broadcast must be hidden"
        );
        assert!(agent_feed.matches(&peer_send), "peer messages stay visible");
    }
    #[tokio::test]
    async fn owner_failure_reaches_filtered_consumer_and_ends_all_readers() {
        use futures::StreamExt;
        let (tx, rx) = mpsc::channel(4);
        let failure = StreamError::OwnerRejected {
            message: "durable replay unavailable".into(),
        };
        tx.send(Err(failure.clone())).await.expect("failure");
        tx.send(Ok(Arc::new(event(RoomId::new()))))
            .await
            .expect("queued sibling event");
        let task = tokio::spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        let mut stream = FilteredEventStream {
            inner: EventStream::daemon(rx, vec![task]),
            filter: EventFilter::default(),
        };
        assert!(matches!(stream.next().await, Some(Err(error)) if error == failure));
        assert!(
            stream.next().await.is_none(),
            "no events after owner refusal"
        );
        assert!(tx.is_closed());
        tokio::task::yield_now().await;
        assert!(abort.is_finished(), "sibling task must terminate");
    }

    #[tokio::test]
    async fn broadcast_lag_remains_a_distinct_recoverable_variant() {
        use futures::StreamExt;
        let (tx, rx) = broadcast::channel(1);
        let mut stream = EventStream::from_broadcast(rx);
        tx.send(Arc::new(event(RoomId::new()))).expect("first");
        tx.send(Arc::new(event(RoomId::new()))).expect("second");
        assert!(matches!(
            stream.next().await,
            Some(Err(StreamError::Lag(LiveLag { skipped: 1 })))
        ));
        assert!(matches!(stream.next().await, Some(Ok(_))));
    }
    #[tokio::test]
    async fn decode_failure_is_terminal_without_becoming_lag() {
        use futures::StreamExt;
        let (tx, rx) = mpsc::channel(1);
        tx.send(Err(StreamError::Decode {
            message: "invalid wire class".into(),
        }))
        .await
        .expect("decode failure");
        let mut stream = EventStream::daemon(rx, Vec::new());
        assert!(
            matches!(stream.next().await, Some(Err(StreamError::Decode { message })) if message == "invalid wire class")
        );
        assert!(stream.next().await.is_none());
        assert!(tx.is_closed());
    }
}
