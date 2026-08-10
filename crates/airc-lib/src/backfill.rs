//! Peer-to-peer transcript backfill — "what did I miss on this channel?"
//!
//! ## Why (#47, incident 2026-08-08)
//!
//! A message published while a peer is unreachable is **lost, not queued**.
//! `airc msg` reports `reached 0 of 76 enrolled peers — NONE currently
//! connected` and that is the end of it: no retry, no forward on reconnect.
//! Measured three times in one day — twice by accident, once inside the
//! investigation of it, when the receiving node was down for a ~4 minute
//! `airc update`.
//!
//! The reason it is lost rather than pending is structural. Replay resumes from
//! the RECEIVER's cursor against the RECEIVER's own store, which is exactly why
//! a daemon restart loses nothing: the events are already local. Across a peer
//! boundary they are not, so a frame that never arrived cannot be replayed —
//! and the receiver has no way to know it missed one.
//!
//! ## Pull, not an outbox
//!
//! The sender-side alternative is an outbox: durably record every unacked
//! event per peer and flush on reconnect. That needs new per-event durable
//! state AND an eviction decision — a queue that grows while a peer is away for
//! a week is its own incident, and an unbounded store with no owner is a defect
//! this project has been bitten by before.
//!
//! Pull needs neither. The sender ALREADY has every event in its durable
//! transcript; the receiver ALREADY holds a per-room cursor. So backfill is
//! "give me events on this channel since my cursor" — the same shape as the
//! daemon's existing resume, crossing a peer boundary instead of a process one.
//! Nothing new is stored, so nothing new needs evicting.
//!
//! `DeliveryLedger` cannot serve this role, incidentally: it is per-peer
//! COUNTERS (attempts, acks, attempts-since-ack), so it can say a peer has not
//! acked in N attempts but not WHICH events it missed. Useful as a trigger,
//! useless as a queue.
//!
//! ## Ask always; do not detect
//!
//! A receiver cannot detect a gap whose frames never arrived — absence of an
//! event leaves no trace to notice. So the contract is to ASK on every
//! peer-connect rather than to ask when a gap is suspected. When the cursor is
//! current the answer is an empty page, which is cheap; detection is the part
//! with no signal, and asking is the part that is always safe.
//!
//! ## Duplicates are free
//!
//! Re-delivering an event the receiver already holds is harmless: events carry
//! a stable `event_id` and the receive path already dedups through the
//! recently-broadcast ring. That is what lets the request be sloppy about its
//! cursor — an overlapping page costs bandwidth, never correctness.
//!
//! Slice 1 (this module) is the request/response pair. Nothing calls it
//! automatically yet; the reconnect watcher that will is slice 2. Landing them
//! separately keeps this one incapable of changing existing behaviour.

use std::time::Duration;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, PeerId, RoomId, TranscriptCursor, TranscriptEvent};
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::Airc;

/// Header marking a command-bus request as a backfill ask. Present on the
/// request; absent on the reply (the reply is correlated by the bus).
pub const HEADER_AIRC_BACKFILL: &str = "airc.backfill";

/// Default ceiling on events returned by one backfill reply.
///
/// Bounded on purpose: a peer returning from a long absence could otherwise ask
/// for a page the size of the whole transcript. When the cap binds, the reply
/// says so (`truncated`) and the caller can ask again from the newer cursor —
/// never a silent cap, which would present a partial history as a complete one.
pub const DEFAULT_BACKFILL_LIMIT: usize = 500;

/// "What did I miss on `channel` since `since`?"
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackfillRequest {
    pub channel: RoomId,
    /// The asker's cursor. `None` means "I have nothing for this channel" —
    /// a fresh scope, or a room it just joined — and yields the newest page.
    pub since: Option<TranscriptCursor>,
    /// Caller's ceiling; the responder clamps to [`DEFAULT_BACKFILL_LIMIT`].
    pub limit: usize,
}

/// The answered page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackfillResponse {
    pub channel: RoomId,
    pub events: Vec<TranscriptEvent>,
    /// True when the limit bound the page — there is MORE beyond it. The
    /// caller re-asks from the newest event it received. Without this a
    /// truncated page is indistinguishable from a complete one, which is how a
    /// gap silently survives the mechanism built to close it.
    pub truncated: bool,
}

impl BackfillRequest {
    /// Recover a request from a received event, or `None` when the event is not
    /// a backfill ask. Total over arbitrary input: a malformed body from a peer
    /// is a non-request, never a panic.
    pub fn from_event(event: &TranscriptEvent) -> Option<Self> {
        event.headers.get(HEADER_AIRC_BACKFILL)?;
        let Some(Body::Json(value)) = event.body.as_ref() else {
            return None;
        };
        serde_json::from_value(value.clone()).ok()
    }
}

impl Airc {
    /// Ask `peer` for everything on `channel` since `since`.
    ///
    /// Returns the events the peer had. An empty page is the ordinary answer
    /// when the cursor is current, NOT an error — which is what makes it safe
    /// to ask on every reconnect.
    pub async fn request_backfill(
        &self,
        peer: PeerId,
        channel: RoomId,
        since: Option<TranscriptCursor>,
        deadline: Duration,
    ) -> Result<BackfillResponse, AircError> {
        let request = BackfillRequest {
            channel,
            since,
            limit: DEFAULT_BACKFILL_LIMIT,
        };
        let mut headers = Headers::new();
        headers.insert(HEADER_AIRC_BACKFILL.into(), "request".into());
        let body = Body::Json(
            serde_json::to_value(&request)
                .map_err(|e| AircError::Crypto(format!("backfill request encode: {e}")))?,
        );
        let pending = self
            .request(MentionTarget::Peer(peer), headers, body, deadline)
            .await?;
        let reply = self.await_reply(pending).await?;
        let Some(Body::Json(value)) = reply.body else {
            return Err(AircError::Crypto(
                "backfill reply carried no JSON body".to_string(),
            ));
        };
        serde_json::from_value(value)
            .map_err(|e| AircError::Crypto(format!("backfill reply decode: {e}")))
    }

    /// Answer a backfill request from this scope's own durable transcript.
    ///
    /// Returns the number of events sent. A request for a channel this scope
    /// does not subscribe to answers with an EMPTY page rather than an error:
    /// "I have nothing for you" is true and actionable, whereas a failure would
    /// make the asker retry against a peer that will never have it.
    pub async fn serve_backfill(
        &self,
        request_event: &TranscriptEvent,
    ) -> Result<usize, AircError> {
        let Some(request) = BackfillRequest::from_event(request_event) else {
            return Ok(0);
        };
        let Some((reply_to, correlation_id)) = crate::command_bus::reply_addressing(request_event)
        else {
            return Ok(0);
        };

        let limit = request.limit.clamp(1, DEFAULT_BACKFILL_LIMIT);
        let events = match request.since.as_ref() {
            Some(cursor) => self
                .daemon_room_transcripts_since(request.channel, cursor, limit)
                .await
                .unwrap_or_default(),
            None => {
                // No cursor: the newest page of the channel.
                let mut filter = crate::EventFilter {
                    channel: Some(request.channel),
                    ..Default::default()
                };
                filter.self_echo = None;
                self.page_recent_filtered(filter, limit)
                    .await
                    .unwrap_or_default()
            }
        };

        let truncated = events.len() >= limit;
        if truncated {
            tracing::debug!(
                target: "airc::backfill",
                channel = %request.channel,
                limit,
                "backfill page hit the limit — replying `truncated` so the asker re-asks"
            );
        }
        let sent = events.len();
        let response = BackfillResponse {
            channel: request.channel,
            events,
            truncated,
        };
        let body = Body::Json(
            serde_json::to_value(&response)
                .map_err(|e| AircError::Crypto(format!("backfill reply encode: {e}")))?,
        );
        self.reply(reply_to, correlation_id, Headers::new(), body)
            .await?;
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::EventId;

    fn request_event(headers: Headers, body: Option<Body>) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::from_uuid(uuid::Uuid::from_u128(7)),
            peer_id: PeerId::from_uuid(uuid::Uuid::from_u128(8)),
            client_id: airc_core::ClientId::new(),
            kind: airc_core::TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000,
            lamport: 1,
            target: MentionTarget::All,
            headers,
            body,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    fn a_request() -> BackfillRequest {
        BackfillRequest {
            channel: RoomId::from_uuid(uuid::Uuid::from_u128(0xB4CF111)),
            since: Some(TranscriptCursor {
                lamport: 42,
                event_id: EventId::new(),
            }),
            limit: 100,
        }
    }

    // what this catches: a peer's malformed or unrelated frame being read as a
    // backfill ask. `serve_backfill` answers from the durable transcript, so a
    // parser that accepted junk would turn any garbage frame into a transcript
    // disclosure. Both the header AND a well-formed body are required.
    #[test]
    fn only_a_well_formed_backfill_frame_parses_as_one() {
        let mut headers = Headers::new();
        headers.insert(HEADER_AIRC_BACKFILL.into(), "request".into());
        let good = Body::Json(serde_json::to_value(a_request()).unwrap());

        assert!(
            BackfillRequest::from_event(&request_event(headers.clone(), Some(good.clone())))
                .is_some()
        );
        assert!(
            BackfillRequest::from_event(&request_event(Headers::new(), Some(good))).is_none(),
            "no header = not a backfill ask, even with a valid body"
        );
        assert!(
            BackfillRequest::from_event(&request_event(
                headers.clone(),
                Some(Body::text("not json"))
            ))
            .is_none(),
            "header without a JSON body must not parse"
        );
        assert!(
            BackfillRequest::from_event(&request_event(
                headers,
                Some(Body::Json(serde_json::json!({"nonsense": true})))
            ))
            .is_none(),
            "header with the WRONG json must not parse"
        );
    }

    // what this catches: the round-trip that carries the whole feature. If
    // `since` stops surviving encode/decode, backfill silently degrades to
    // "newest page" — it would still return events, still look like it worked,
    // and quietly stop closing the gap it exists to close.
    #[test]
    fn a_request_survives_the_wire_with_its_cursor_intact() {
        let original = a_request();
        let encoded = serde_json::to_value(&original).unwrap();
        let decoded: BackfillRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(
            decoded.since.as_ref().map(|c| c.lamport),
            Some(42),
            "the cursor is the whole request — losing it makes the answer arbitrary"
        );
    }

    // what this catches: a truncated page presented as a complete one. Without
    // `truncated`, a peer returning from a long absence gets the oldest N events
    // it missed and no indication that more exist — the gap survives the
    // mechanism built to close it, which is worse than not backfilling at all
    // because it looks fixed.
    #[test]
    fn a_bounded_page_says_that_it_was_bounded() {
        let full = BackfillResponse {
            channel: RoomId::from_uuid(uuid::Uuid::from_u128(1)),
            events: Vec::new(),
            truncated: true,
        };
        let decoded: BackfillResponse =
            serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
        assert!(
            decoded.truncated,
            "the `there is more` bit must survive the wire"
        );
    }
}
