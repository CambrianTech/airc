use std::collections::{BTreeSet, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use airc_core::transcript::TranscriptKind;
use airc_core::{HeaderFilter, RoomId, TranscriptEvent};
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
    Broadcast(BroadcastStream<Arc<TranscriptEvent>>),
    Daemon {
        rx: ReceiverStream<Arc<TranscriptEvent>>,
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
        rx: mpsc::Receiver<Arc<TranscriptEvent>>,
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
    type Item = Result<Arc<TranscriptEvent>, LiveLag>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match &mut this.inner {
            EventStreamInner::Broadcast(stream) => match Pin::new(stream).poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(event))),
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(n)))) => {
                    Poll::Ready(Some(Err(LiveLag { skipped: n })))
                }
                Poll::Ready(None) => Poll::Ready(None),
            },
            // The daemon attach loop handles lag internally (resume-from-
            // cursor on the IPC side), so the SDK consumer only ever sees
            // delivered events here.
            EventStreamInner::Daemon { rx, .. } => match Pin::new(rx).poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Some(event)) => Poll::Ready(Some(Ok(event))),
                Poll::Ready(None) => Poll::Ready(None),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLag {
    pub skipped: u64,
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
    pub channel: Option<RoomId>,
    pub channels: HashSet<RoomId>,
    pub kinds: BTreeSet<TranscriptKind>,
    pub headers_filter: HeaderFilter,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            channel: None,
            channels: HashSet::new(),
            kinds: BTreeSet::new(),
            headers_filter: HeaderFilter::Any,
        }
    }
}

impl EventFilter {
    pub fn current_room() -> Self {
        Self::default()
    }

    pub fn matches(&self, event: &TranscriptEvent) -> bool {
        if let Some(channel) = self.channel {
            if event.room_id != channel {
                return false;
            }
        }
        if !self.channels.is_empty() && !self.channels.contains(&event.room_id) {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&event.kind) {
            return false;
        }
        self.headers_filter.matches(&event.headers)
    }
}

pub struct FilteredEventStream {
    pub(crate) inner: EventStream,
    pub(crate) filter: EventFilter,
}

impl Stream for FilteredEventStream {
    type Item = Result<Arc<TranscriptEvent>, LiveLag>;

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
    use std::collections::{BTreeSet, HashSet};

    use airc_core::{Body, ClientId, EventId, Headers, MentionTarget, PeerId};

    use super::*;

    fn event(room_id: RoomId, kind: TranscriptKind) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id,
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind,
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
    fn event_filter_uses_channel_membership_set() {
        let admitted = RoomId::from_u128(0x01);
        let other = RoomId::from_u128(0x02);
        let filter = EventFilter {
            channel: None,
            channels: HashSet::from([admitted]),
            kinds: BTreeSet::new(),
            headers_filter: HeaderFilter::Any,
        };

        assert!(filter.matches(&event(admitted, TranscriptKind::Message)));
        assert!(!filter.matches(&event(other, TranscriptKind::Message)));
    }
}
