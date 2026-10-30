//! Convert a wire-level `Frame` into the canonical `TranscriptEvent`.
//!
//! This is the substrate's persistence boundary: `Frame` is what the
//! transport carries; `TranscriptEvent` is what gets durably stored
//! and replayed via the event store. Every frame converts EXCEPT an
//! addressed one (`FrameKind::Addressed`), which has no transcript form:
//! it is readable by its two ends only and is never stored on any
//! machine. The conversion deliberately
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
    pub fn into_transcript_event(self) -> Result<TranscriptEvent, NotTranscribable> {
        let Frame { kind, envelope } = self;
        // The kind label sits outside the envelope signature, so a relay can
        // relabel an addressed frame; its reserved channel cannot be moved
        // without breaking the signature, so either one refuses.
        if envelope.channel == crate::addressed::addressed_channel()
            || kind != FrameKind::for_class(envelope.class)
        {
            return Err(NotTranscribable);
        }
        let kind = match kind {
            FrameKind::Addressed => return Err(NotTranscribable),
            FrameKind::Message => TranscriptKind::Message,
            FrameKind::Event => TranscriptKind::System,
            FrameKind::Control => TranscriptKind::SessionControl,
            FrameKind::Command => TranscriptKind::Command,
            FrameKind::CommandResult => TranscriptKind::CommandResult,
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

        Ok(TranscriptEvent {
            class: envelope.class,
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
        })
    }
}

/// An addressed frame offered for transcription. Addressed traffic never
/// enters a transcript; reaching this is a routing defect upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotTranscribable;

impl std::fmt::Display for NotTranscribable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "an addressed frame has no transcript form")
    }
}

impl std::error::Error for NotTranscribable {}

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
                class: airc_core::EventClass::Correspondence(
                    airc_core::CorrespondenceKind::Message,
                ),
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
        let event = frame.into_transcript_event().unwrap();
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
        for (wire_kind, event_kind) in [
            (FrameKind::Command, TranscriptKind::Command),
            (FrameKind::CommandResult, TranscriptKind::CommandResult),
        ] {
            let mut frame = message_frame();
            frame.kind = wire_kind;
            frame.envelope.class = airc_core::EventClass::Control(match wire_kind {
                FrameKind::Command => airc_core::ControlKind::Command,
                FrameKind::CommandResult => airc_core::ControlKind::CommandResult,
                _ => unreachable!("test cases are commands"),
            });
            // Deliberately opaque bytes: classification cannot inspect JSON.
            frame.envelope.body = None;
            assert_eq!(frame.into_transcript_event().unwrap().kind, event_kind);
        }
        let mut frame = message_frame();
        frame.kind = FrameKind::Event;
        frame.envelope.class = airc_core::EventClass::Activity(airc_core::ActivityKind::WorkState);
        assert_eq!(
            frame.into_transcript_event().unwrap().kind,
            TranscriptKind::System,
            "event-kind preserves as system rather than message"
        );

        let mut frame = message_frame();
        frame.kind = FrameKind::Control;
        frame.envelope.class = airc_core::EventClass::Control(airc_core::ControlKind::Session);
        assert_eq!(
            frame.into_transcript_event().unwrap().kind,
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
        let event = frame.into_transcript_event().unwrap();
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
        let event: TranscriptEvent = message_frame().into_transcript_event().unwrap();
        let json = serde_json::to_value(&event).unwrap();
        assert!(
            json.get("signature").is_none(),
            "TranscriptEvent must not carry a signature field"
        );
    }

    // what this catches: an addressed frame (a sealed command or a handshake)
    // has no transcript form, so no store or ring can be handed one by a
    // caller that forgot to route it first.
    #[test]
    fn an_addressed_frame_is_not_transcribable() {
        let mut frame = message_frame();
        frame.kind = FrameKind::Addressed;
        assert_eq!(frame.into_transcript_event().err(), Some(NotTranscribable));
    }

    // what this catches: review F4. A relay relabels an addressed frame as a
    // Message (the kind is outside the signature); its reserved channel still
    // keeps it out of every transcript.
    #[test]
    fn a_relabelled_addressed_frame_is_not_transcribable() {
        let mut frame = message_frame();
        frame.envelope.channel = crate::addressed::addressed_channel();
        assert_eq!(frame.into_transcript_event().err(), Some(NotTranscribable));
    }
}
