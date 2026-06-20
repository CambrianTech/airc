//! Convert a wire-level `Frame` into the canonical `TranscriptEvent`.
//!
//! This is the substrate's persistence boundary: `Frame` is what the
//! transport carries; `TranscriptEvent` is what gets durably stored
//! and replayed via the event store. The conversion is total — every
//! Frame can be turned into a TranscriptEvent — but it deliberately
//! drops signatures (verified upstream by `SignedTransport`; not part
//! of the durable record) and reply-to (re-projected into headers if
//! callers need it).
//!
//! Lives in airc-protocol rather than airc-core because the function
//! body touches `Frame`, and Frame lives here.

use airc_core::transcript::{TranscriptEvent, TranscriptKind};
use serde_json::Value as JsonValue;

use crate::envelope::{Frame, FrameKind};
use crate::headers_keys::{HEADER_AIRC_MEDIA, HEADER_AIRC_REPLY_TO};

impl Frame {
    /// Convert this frame into a `TranscriptEvent` for durable storage.
    ///
    /// Mapping rules:
    ///   - `event_id`, `sender → peer_id`, `sender_client → client_id`,
    ///     `channel → room_id`, `target`, `lamport`, `occurred_at_ms`,
    ///     `headers`, `body` are 1:1.
    ///   - `FrameKind::Message → TranscriptKind::Message`.
    ///   - `FrameKind::Event → TranscriptKind::System` (no direct
    ///     transcript equivalent; system-kind preserves it as an
    ///     auditable record without claiming it's a chat message).
    ///   - `FrameKind::Control → TranscriptKind::SessionControl`.
    ///   - `signature` is dropped — verified before persistence; the
    ///     store doesn't need it again.
    ///   - `reply_to` is projected into `metadata["airc.reply_to"]`
    ///     so consumers can recover it without losing data.
    ///   - `media` is preserved in `metadata["airc.media"]` as a JSON
    ///     array (full attachment carry-through is a follow-up; we
    ///     don't drop the data, we just don't promote it to the
    ///     `attachment` field yet).
    pub fn into_transcript_event(self) -> TranscriptEvent {
        let Frame { kind, envelope } = self;
        let kind = match kind {
            FrameKind::Message => TranscriptKind::Message,
            FrameKind::Event => TranscriptKind::System,
            FrameKind::Control => TranscriptKind::SessionControl,
        };

        let mut metadata = serde_json::Map::new();
        if let Some(reply_to) = envelope.reply_to {
            metadata.insert(
                HEADER_AIRC_REPLY_TO.to_string(),
                serde_json::to_value(reply_to).unwrap_or(JsonValue::Null),
            );
        }
        if !envelope.media.is_empty() {
            metadata.insert(
                HEADER_AIRC_MEDIA.to_string(),
                serde_json::to_value(&envelope.media).unwrap_or(JsonValue::Null),
            );
        }

        TranscriptEvent {
            event_id: envelope.event_id,
            room_id: envelope.channel,
            peer_id: envelope.sender,
            client_id: envelope.sender_client,
            kind,
            occurred_at_ms: envelope.occurred_at_ms,
            lamport: envelope.lamport,
            target: envelope.target,
            headers: envelope.headers,
            body: envelope.body,
            attachment: None,
            receipt: None,
            metadata: JsonValue::Object(metadata),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, FrameKind};
    use crate::signature::Signature;
    use airc_core::{
        body::Body, headers::Headers, transcript::MentionTarget, ClientId, EventId, PeerId, RoomId,
    };

    fn message_frame() -> Frame {
        Frame {
            kind: FrameKind::Message,
            envelope: Envelope {
                event_id: EventId::from_u128(0x01),
                sender: PeerId::from_u128(0xa1),
                sender_client: ClientId::from_u128(0xc1),
                channel: RoomId::from_u128(0xc0ffee),
                target: MentionTarget::All,
                lamport: 7,
                occurred_at_ms: 1_700_000_000_000,
                reply_to: None,
                headers: Headers::new(),
                body: Some(Body::text("hi")),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    #[test]
    fn message_frame_round_trips_into_transcript_event() {
        // The minimum-viable contract: all identity, ordering, and
        // body fields survive the boundary.
        let frame = message_frame();
        let expected = frame.envelope.clone();
        let event = frame.into_transcript_event();
        assert_eq!(event.event_id, expected.event_id);
        assert_eq!(event.peer_id, expected.sender);
        assert_eq!(event.client_id, expected.sender_client);
        assert_eq!(event.room_id, expected.channel);
        assert_eq!(event.lamport, expected.lamport);
        assert_eq!(event.occurred_at_ms, expected.occurred_at_ms);
        assert_eq!(event.target, expected.target);
        assert_eq!(event.headers, expected.headers);
        assert_eq!(event.body, expected.body);
        assert_eq!(event.kind, TranscriptKind::Message);
    }

    #[test]
    fn frame_kind_maps_to_transcript_kind() {
        let mut frame = message_frame();
        frame.kind = FrameKind::Event;
        assert_eq!(
            frame.into_transcript_event().kind,
            TranscriptKind::System,
            "event-kind preserves as system rather than message"
        );

        let mut frame = message_frame();
        frame.kind = FrameKind::Control;
        assert_eq!(
            frame.into_transcript_event().kind,
            TranscriptKind::SessionControl
        );
    }

    #[test]
    fn reply_to_is_projected_into_metadata() {
        // Substrate contract: dropping reply_to silently would lose
        // threading info. The conversion projects it into metadata
        // under the substrate-owned key so consumers can recover it.
        let mut frame = message_frame();
        let reply_target = EventId::from_u128(0xabc);
        frame.envelope.reply_to = Some(reply_target);
        let event = frame.into_transcript_event();
        let recovered: Option<EventId> = event
            .metadata
            .get(HEADER_AIRC_REPLY_TO)
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        assert_eq!(recovered, Some(reply_target));
    }

    #[test]
    fn signature_does_not_appear_in_stored_event() {
        // Signatures are verified at the transport boundary; the
        // durable record is the canonical TranscriptEvent and has no
        // signature field. This test is structural — if a future
        // refactor accidentally adds a `signature` field to
        // TranscriptEvent, the call below stops compiling.
        let event: TranscriptEvent = message_frame().into_transcript_event();
        let json = serde_json::to_value(&event).unwrap();
        assert!(
            json.get("signature").is_none(),
            "TranscriptEvent must not carry a signature field"
        );
    }
}
