//! Daemon-attached SDK mode.
//!
//! Consumers still use `Airc`; attach mode routes operations through the
//! daemon's typed IPC client. The daemon is the owner-core: it publishes
//! to its `EventRouter` and answers reads from the router (hot ring +
//! SQLite durable tier). Events cross the IPC boundary as opaque
//! **airc-wire bytes** (`airc_bus::Envelope`); this bridge decodes them
//! once and projects to the SDK's [`TranscriptEvent`] for consumers.
//!
//! Ordering: the owner-core order is the generational `(epoch, counter)`
//! seq. The SDK's `lamport`/`TranscriptCursor` is a single monotonic
//! `u64`, so we pack the seq losslessly — `epoch` in the high bits,
//! `counter` in the low [`COUNTER_BITS`]. This keeps `lamport` monotonic
//! (resume works) without a lamport→seq lossy shim.

use std::sync::Arc;

use airc_bus::envelope::{Envelope, Kind, Target};
use airc_core::{Body, MentionTarget, RoomId, TranscriptCursor, TranscriptEvent, TranscriptKind};
use airc_ipc::codec::read_frame;
use airc_ipc::{
    AttachRequest, AttachStart, InboxRequest, IpcCursor, IpcDelivery, IpcTarget, PublishRequest,
    Response, SendRequest,
};
use airc_protocol::FrameKind;
use tokio::sync::mpsc;

use crate::error::AircError;
use crate::publish::PublishReceipt;
use crate::room::Room;
use crate::stream::EventStream;
use crate::Airc;

/// Low bits of the packed `lamport` that hold the per-epoch counter; the
/// remaining high bits hold the epoch. 40 bits ⇒ ~1.1e12 events per
/// epoch and ~16.7M epochs (restarts) — neither realistically reachable.
//
// Re-exported from airc-ipc so the wire vocabulary and the SDK
// projection can never disagree on layout. Anyone reading
// `COUNTER_BITS` here gets the same value as the IPC From impls.
use airc_ipc::{COUNTER_BITS, COUNTER_MASK};

/// Reconnect backoff for a daemon attach stream that drops (daemon
/// restart / transient loss): start small, double, cap — so a long
/// outage doesn't hot-loop. On reconnect the subscription resumes
/// strictly after the last delivered cursor (durable gap replayed,
/// ephemeral correctly skipped).
const RECONNECT_BACKOFF_START_MS: u64 = 100;
const RECONNECT_BACKOFF_MAX_MS: u64 = 2_000;

/// Pack a generational `(epoch, counter)` seq into a single monotonic
/// `lamport`. Higher epoch ⇒ higher value; within an epoch, higher
/// counter ⇒ higher value — so the SDK's lamport ordering matches the
/// owner-core total order.
fn pack_seq(epoch: u64, counter: u64) -> u64 {
    (epoch << COUNTER_BITS) | (counter & COUNTER_MASK)
}

/// Inverse of [`pack_seq`].
fn unpack_seq(lamport: u64) -> (u64, u64) {
    (lamport >> COUNTER_BITS, lamport & COUNTER_MASK)
}

fn kind_to_transcript(kind: Kind) -> TranscriptKind {
    match kind {
        Kind::Message => TranscriptKind::Message,
        // The fine-grained transcript kind for non-chat envelopes is
        // carried in headers; project the coarse bus kind to System.
        Kind::Event
        | Kind::Command
        | Kind::CommandResult
        | Kind::Signal
        | Kind::StreamChunk
        | Kind::Control => TranscriptKind::System,
    }
}

fn target_to_mention(target: &Target) -> MentionTarget {
    match target {
        Target::Peer(peer) => MentionTarget::Peer(*peer),
        // `Endpoint("room:<uuid>")` is the round-trip of a room mention.
        Target::Endpoint(name) => name
            .strip_prefix("room:")
            .and_then(|uuid| uuid.parse().ok())
            .map(|uuid| MentionTarget::Room(RoomId::from_uuid(uuid)))
            .unwrap_or(MentionTarget::All),
        Target::All | Target::Reply(_) | Target::Capability(_) => MentionTarget::All,
    }
}

/// Project a decoded owner-core envelope to the SDK transcript shape.
fn project(env: &Envelope) -> TranscriptEvent {
    TranscriptEvent {
        event_id: env.event_id,
        room_id: env.channel,
        peer_id: env.from.0,
        client_id: env.from.1,
        kind: kind_to_transcript(env.kind),
        occurred_at_ms: env.occurred_at_ms,
        lamport: pack_seq(env.seq.epoch, env.seq.counter),
        target: target_to_mention(&env.target),
        headers: env.headers.clone(),
        // Durable chat payloads are `Body`-encoded; a non-`Body` payload
        // (raw stream chunk) has no transcript body.
        body: Body::from_payload(&env.payload).ok(),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}

/// Decode an airc-wire envelope buffer (as carried by `Response::Event`
/// / `InboxResponse.envelopes`) and project it to the SDK transcript
/// shape. A malformed buffer is surfaced as an error, never silently
/// dropped. Public so live-feed consumers (the CLI monitor) decode
/// daemon events without re-implementing the projection.
pub fn decode_wire_event(bytes: Vec<u8>) -> Result<TranscriptEvent, AircError> {
    let env = airc_wire::decode(bytes.into())
        .map_err(|e| AircError::Route(format!("daemon event decode: {e}")))?;
    Ok(project(&env))
}

impl Airc {
    pub(crate) fn daemon_client(&self) -> Option<&airc_ipc::DaemonClient> {
        self.inner.daemon_client.as_deref()
    }

    fn require_daemon_client(&self) -> Result<&airc_ipc::DaemonClient, AircError> {
        self.daemon_client()
            .ok_or_else(|| AircError::Route("daemon client is not attached".to_string()))
    }

    pub(crate) async fn daemon_send_text(
        &self,
        room: &Room,
        text: &str,
        headers: airc_core::Headers,
    ) -> Result<airc_core::EventId, AircError> {
        let receipt = self
            .require_daemon_client()?
            .send(SendRequest {
                channel: room.channel.as_uuid(),
                from_peer: self.peer_id().as_uuid(),
                from_client: self.client_id().as_uuid(),
                text: text.to_string(),
                headers,
            })
            .await?;
        // The owner-core assigns the real event id at publish; return it.
        Ok(receipt.event_id)
    }

    /// Publish a frame through the daemon with full addressing — the
    /// daemon-attached path for `send_frame_to_room` (so `publish`,
    /// work events, lifecycle, and any structured send route through the
    /// router, not just `say`). Returns the owner-assigned metadata.
    pub(crate) async fn daemon_send_frame(
        &self,
        room: &Room,
        kind: FrameKind,
        target: MentionTarget,
        body: Body,
        headers: airc_core::Headers,
    ) -> Result<crate::messaging::SendFrameResult, AircError> {
        let response = self
            .require_daemon_client()?
            .publish(PublishRequest {
                channel: room.channel.as_uuid(),
                from_peer: self.peer_id().as_uuid(),
                from_client: self.client_id().as_uuid(),
                kind: kind.into(),
                delivery: IpcDelivery::Durable,
                target: target.into(),
                correlation_id: None,
                coalesce_key: None,
                payload: body.to_payload(),
                headers,
            })
            .await?;
        Ok(crate::messaging::SendFrameResult {
            event_id: response.event_id,
            lamport: pack_seq(response.epoch, response.counter),
            occurred_at_ms: response.occurred_at_ms,
        })
    }

    pub(crate) async fn daemon_publish(
        &self,
        room: &Room,
        kind: FrameKind,
        body: Body,
        headers: airc_core::Headers,
    ) -> Result<PublishReceipt, AircError> {
        let response = self
            .require_daemon_client()?
            .publish(PublishRequest {
                channel: room.channel.as_uuid(),
                from_peer: self.peer_id().as_uuid(),
                from_client: self.client_id().as_uuid(),
                kind: kind.into(),
                // SDK chat/structured publishes are durable; the live
                // streaming classes are reached via the typed IPC client
                // directly (media/game-state), not this chat helper.
                delivery: IpcDelivery::Durable,
                target: IpcTarget::All,
                correlation_id: None,
                coalesce_key: None,
                // The consumer's `Body` is encoded to opaque payload
                // bytes here; the daemon routes them without parsing.
                payload: body.to_payload(),
                headers,
            })
            .await?;
        Ok(PublishReceipt {
            event_id: response.event_id,
            lamport: pack_seq(response.epoch, response.counter),
            occurred_at_ms: response.occurred_at_ms,
            channel_id: response.channel_id,
            channel_name: room.name.clone(),
        })
    }

    pub(crate) async fn daemon_page_recent(
        &self,
        room: &Room,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let response = self
            .require_daemon_client()?
            .inbox(InboxRequest {
                since: None,
                channel: Some(room.channel),
                limit: Some(limit),
            })
            .await?;
        response
            .envelopes
            .into_iter()
            .map(decode_wire_event)
            .collect()
    }

    pub(crate) async fn daemon_resume_from(
        &self,
        room: &Room,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let (epoch, counter) = unpack_seq(cursor.lamport);
        let response = self
            .require_daemon_client()?
            .inbox(InboxRequest {
                since: Some(IpcCursor {
                    epoch,
                    counter,
                    event_id: cursor.event_id,
                }),
                channel: Some(room.channel),
                limit: Some(limit),
            })
            .await?;
        response
            .envelopes
            .into_iter()
            .map(decode_wire_event)
            .collect()
    }

    /// Most-recent `limit` events across ALL subscribed rooms, via the
    /// daemon. The owner-core router pages per channel, so this fans the
    /// query over the subscription set and merges by total order
    /// `(lamport, event_id)` — the daemon-backed equivalent of the
    /// `page_recent_subscribed` direct path.
    pub(crate) async fn daemon_page_recent_subscribed(
        &self,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let set = self.subscription_set().await?;
        let mut merged = Vec::new();
        for subscription in set.all() {
            let room = subscription.as_room();
            merged.extend(self.daemon_page_recent(&room, limit).await?);
        }
        merged.sort_by_key(|event| event.lamport);
        if merged.len() > limit {
            merged = merged.split_off(merged.len() - limit);
        }
        Ok(merged)
    }

    /// Events strictly after `cursor` across ALL subscribed rooms, via
    /// the daemon. Same per-channel fan + merge as
    /// [`Airc::daemon_page_recent_subscribed`], resuming each room from
    /// the shared `(epoch, counter)` packed in the cursor.
    pub(crate) async fn daemon_resume_from_subscribed(
        &self,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let set = self.subscription_set().await?;
        let mut merged = Vec::new();
        for subscription in set.all() {
            let room = subscription.as_room();
            merged.extend(self.daemon_resume_from(&room, cursor, limit).await?);
        }
        merged.sort_by_key(|event| event.lamport);
        merged.truncate(limit);
        Ok(merged)
    }

    /// Fetch ALL transcript events on `channel` via the daemon, paging
    /// forward from the start. The daemon-attached source for projections
    /// that need the complete history (e.g. the work board).
    pub(crate) async fn daemon_room_transcripts(
        &self,
        channel: RoomId,
        page_size: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let page_size = page_size.max(1);
        let client = self.require_daemon_client()?;
        let mut all = Vec::new();
        // Start strictly after the zero cursor ⇒ from the first event
        // (epochs begin at 1, so (0,0) precedes everything).
        let mut since = Some(IpcCursor {
            epoch: 0,
            counter: 0,
            event_id: airc_core::EventId::from_u128(0),
        });
        loop {
            let response = client
                .inbox(InboxRequest {
                    since,
                    channel: Some(channel),
                    limit: Some(page_size),
                })
                .await?;
            let count = response.envelopes.len();
            for bytes in response.envelopes {
                all.push(decode_wire_event(bytes)?);
            }
            if count < page_size {
                break;
            }
            match response.newest {
                Some(cursor) => since = Some(cursor),
                None => break,
            }
        }
        Ok(all)
    }

    /// Live subscribe across `channels` via the daemon: open one IPC
    /// attach per channel, decode each into a `TranscriptEvent`, and
    /// merge them into a single [`EventStream`]. The attach tasks are
    /// owned by the stream (aborted on drop). Each attach is registered
    /// at the live edge before we move on (subscribe-before-ack on the
    /// daemon side), so no early event is missed.
    pub(crate) async fn daemon_subscribe(
        &self,
        channels: Vec<RoomId>,
    ) -> Result<EventStream, AircError> {
        // Own the client so each reader task can re-attach after a daemon
        // restart (the borrow from `require_daemon_client` can't outlive
        // this call).
        let client = self
            .inner
            .daemon_client
            .clone()
            .ok_or_else(|| AircError::Route("daemon client is not attached".to_string()))?;
        let (tx, rx) = mpsc::channel::<Arc<TranscriptEvent>>(1024);
        let mut handles = Vec::with_capacity(channels.len());
        for channel in channels {
            // Initial attach + ack — fail fast so `subscribe()` errors if
            // the daemon is down right now (don't silently spin).
            //
            // Card bf0b5790: this is the LIVE subscribe surface, so the
            // stream starts at the live edge. Catch-up is a separate,
            // bounded concern (`resume_from_subscribed_filtered` with a
            // stored cursor — see `join_feed`).
            let mut stream = client
                .attach(AttachRequest::live(channel))
                .await
                .map_err(|e| AircError::Route(format!("daemon attach: {e}")))?;
            match read_frame::<_, Response>(&mut stream).await {
                Ok(Some(Response::Ok)) => {}
                Ok(Some(Response::Error { message })) => {
                    return Err(AircError::Route(format!("daemon attach: {message}")))
                }
                Ok(Some(_)) | Ok(None) => {
                    return Err(AircError::Route("daemon attach: no ack".to_string()))
                }
                Err(e) => return Err(AircError::Route(format!("daemon attach ack: {e}"))),
            }
            let tx = tx.clone();
            let client = client.clone();
            handles.push(tokio::spawn(async move {
                // Drain the live stream; when it drops (daemon restart /
                // transient loss) re-attach and RESUME strictly after the
                // last delivered cursor. Durable events in the gap are
                // replayed; ephemeral classes are lossy and correctly
                // skipped. The `DaemonAttachGuard` aborts this task when
                // the consumer drops the `EventStream`, so it only runs
                // while the subscription is wanted.
                let mut from: Option<IpcCursor> = None;
                let mut backoff_ms = RECONNECT_BACKOFF_START_MS;
                loop {
                    loop {
                        match read_frame::<_, Response>(&mut stream).await {
                            Ok(Some(Response::Event { envelope })) => {
                                match decode_wire_event(envelope) {
                                    Ok(event) => {
                                        from = Some(cursor_after(&event));
                                        if tx.send(Arc::new(event)).await.is_err() {
                                            return; // consumer gone
                                        }
                                        backoff_ms = RECONNECT_BACKOFF_START_MS;
                                    }
                                    Err(error) => {
                                        // Card 807193ab: a silent return here
                                        // killed the subscription with no
                                        // diagnostic — the consumer's mpsc
                                        // would close and `next().await` just
                                        // yielded None. Now operators see
                                        // WHY the substrate dropped the
                                        // subscription (wire schema drift,
                                        // encoding bug, anything that breaks
                                        // decode).
                                        tracing::warn!(
                                            channel = %channel,
                                            error = %error,
                                            "airc subscribe: dropping subscription — decode_wire_event failed"
                                        );
                                        return;
                                    }
                                }
                            }
                            Ok(Some(other)) => {
                                // Non-Event frames on a live subscription
                                // are unexpected (ack already consumed); a
                                // recurring stream of these indicates the
                                // daemon is sending shapes the SDK doesn't
                                // recognise.
                                tracing::warn!(
                                    channel = %channel,
                                    frame = ?other,
                                    "airc subscribe: ignoring non-Event frame"
                                );
                            }
                            Ok(None) | Err(_) => {
                                // Card 807193ab: surface stream drops so a
                                // session of "no events" can be told apart
                                // from "connection died, reconnecting."
                                tracing::warn!(
                                    channel = %channel,
                                    "airc subscribe: stream closed — reconnecting"
                                );
                                break;
                            }
                        }
                    }
                    // Reconnect with resume + capped backoff.
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        backoff_ms = (backoff_ms * 2).min(RECONNECT_BACKOFF_MAX_MS);
                        if tx.is_closed() {
                            return; // consumer dropped while we were down
                        }
                        // Card bf0b5790: a drop BEFORE the first
                        // delivered event leaves no cursor — re-attach
                        // at the live edge rather than replaying the
                        // transcript. With a cursor, resume the gap
                        // exactly as before.
                        let start = match from {
                            Some(cursor) => AttachStart::After(cursor),
                            None => AttachStart::Live,
                        };
                        let mut s = match client
                            .attach(AttachRequest::new(channel, start))
                            .await
                        {
                            Ok(s) => s,
                            Err(error) => {
                                // Card 807193ab: silent `continue` left
                                // operators watching a dead connection with
                                // no signal. Show the reattach failure +
                                // current backoff.
                                tracing::warn!(
                                    channel = %channel,
                                    backoff_ms,
                                    error = %error,
                                    "airc subscribe: reattach failed"
                                );
                                continue;
                            }
                        };
                        match read_frame::<_, Response>(&mut s).await {
                            Ok(Some(Response::Ok)) => {
                                stream = s;
                                backoff_ms = RECONNECT_BACKOFF_START_MS;
                                break; // reconnected; resume draining
                            }
                            Ok(Some(other)) => {
                                tracing::warn!(
                                    channel = %channel,
                                    frame = ?other,
                                    "airc subscribe: reattach ack mismatch — expected Ok"
                                );
                            }
                            Ok(None) | Err(_) => {
                                tracing::warn!(
                                    channel = %channel,
                                    backoff_ms,
                                    "airc subscribe: reattach ack read failed"
                                );
                            }
                        }
                    }
                }
            }));
        }
        Ok(EventStream::daemon(rx, handles))
    }
}

/// The resume cursor for the next attach: the daemon replays strictly
/// after this point, so tracking the last delivered event makes a
/// reconnect gap-free for durable events.
fn cursor_after(event: &TranscriptEvent) -> IpcCursor {
    let (epoch, counter) = unpack_seq(event.lamport);
    IpcCursor {
        epoch,
        counter,
        event_id: event.event_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_packs_and_unpacks_losslessly_and_orders() {
        for (epoch, counter) in [(0, 0), (1, 5), (3, COUNTER_MASK), (42, 1_000_000)] {
            let packed = pack_seq(epoch, counter);
            assert_eq!(unpack_seq(packed), (epoch, counter));
        }
        // Monotonic: higher epoch outranks any counter in a lower epoch.
        assert!(pack_seq(2, 0) > pack_seq(1, COUNTER_MASK));
        // Within an epoch, higher counter ranks higher.
        assert!(pack_seq(1, 10) > pack_seq(1, 9));
    }
}
