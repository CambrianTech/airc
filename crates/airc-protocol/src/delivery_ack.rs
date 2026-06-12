//! Delivery-ack vocabulary for point-to-point sends (card 39d37629).
//!
//! ## The bug this closes
//!
//! `airc lan-send` used to print "sent" the moment the frame bytes
//! were flushed to the TLS socket — write-success, NOT delivery. A
//! frame could be accepted by the remote TLS listener and then vanish
//! before room-transcript persistence (decode failure on a skewed
//! build, signature-verification failure, store error, or persistence
//! into a store no bound channel reads), with zero signal on either
//! side. Live repro 2026-06-12 02:36Z: 5090 → mac `lan-send` printed
//! "sent over lan-tcp to general", exit 0; the frame appears in NO
//! store on the receiving machine.
//!
//! ## The contract
//!
//! A sender that wants a delivery receipt sets the
//! [`HEADER_AIRC_DELIVERY_ACK`] header to [`DELIVERY_ACK_REQUEST`] on
//! an ordinary frame. A receiver that understands the header responds
//! on the same connection with a `FrameKind::Control` frame whose
//! headers carry [`DELIVERY_ACK_RESPONSE`] and whose JSON body is a
//! [`DeliveryAck`] — emitted only AFTER the persistence decision, so
//! `delivered` means "durably in my transcript store on a channel this
//! scope has bound", never "accepted".
//!
//! ## Wire compatibility
//!
//! No new `FrameKind` and no new top-level wire shape: both the
//! request marker and the response travel as plain headers + JSON body
//! on existing frame kinds, so peers running older builds decode them
//! as ordinary frames. An old receiver simply never responds — the new
//! sender times out and reports "no ack" distinctly (the frame may
//! still have been delivered). An old sender never sets the request
//! header, so it never receives ack frames at all. The JSON shapes
//! below are pinned by literal-JSON tests; renaming any field or
//! variant breaks the pins (mutation-verified).

use serde::{Deserialize, Serialize};

use airc_core::{Body, EventId, PeerId, RoomId, TranscriptCursor};

use crate::envelope::Frame;

/// Header key marking a frame as ack-requesting (value
/// [`DELIVERY_ACK_REQUEST`]) or as an ack response (value
/// [`DELIVERY_ACK_RESPONSE`]). Substrate-owned `airc.*` namespace.
pub const HEADER_AIRC_DELIVERY_ACK: &str = "airc.delivery_ack";

/// Header value: "sender requests a delivery ack for this frame."
pub const DELIVERY_ACK_REQUEST: &str = "request";

/// Header value: "this frame's body is a [`DeliveryAck`]."
pub const DELIVERY_ACK_RESPONSE: &str = "response";

/// Typed delivery receipt — the JSON body of an ack-response frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryAck {
    /// The `event_id` of the frame this ack answers.
    pub for_event: EventId,
    /// The peer that produced this ack (the receiving side).
    pub receiver: PeerId,
    /// What happened to the frame on the receiving side.
    pub outcome: DeliveryOutcome,
}

/// Receiver-side outcome for an ack-requesting frame. `Delivered` is
/// emitted only after the frame is durably persisted AND addressed to
/// a channel the receiving scope has bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// Persisted into the receiver's transcript store on a bound
    /// channel. `cursor` is the receiver-side transcript position.
    Delivered {
        channel: RoomId,
        cursor: TranscriptCursor,
    },
    /// Accepted but NOT delivered to any bound room transcript.
    Undeliverable { reason: UndeliverableReason },
}

/// Why an accepted frame could not be delivered. Mirrors the
/// `frame_undeliverable` diagnostic's reason vocabulary; only the
/// reasons a receiver can still respond to travel on the wire
/// (decode/verification failures kill the frame before the receiver
/// can correlate an ack — the sender sees those as ack timeouts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndeliverableReason {
    /// The frame's channel is not bound in the receiving scope's
    /// subscription set — no transcript surface will ever show it.
    UnknownChannel,
    /// The receiver's durable store rejected the append.
    PersistFailed,
    /// The receiving adapter had no subscriber to hand the frame to.
    NoSubscriber,
    /// The frame bytes did not decode (diagnostic-only; never acked).
    DecodeFailure,
    /// The frame failed signature verification (diagnostic-only;
    /// never acked).
    VerificationFailed,
}

impl UndeliverableReason {
    /// Stable snake_case label — used in diagnostics fields and CLI
    /// output. Matches the serde wire encoding by construction (the
    /// pin tests assert both).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownChannel => "unknown_channel",
            Self::PersistFailed => "persist_failed",
            Self::NoSubscriber => "no_subscriber",
            Self::DecodeFailure => "decode_failure",
            Self::VerificationFailed => "verification_failed",
        }
    }
}

/// Does this frame ask its receiver for a delivery ack?
pub fn wants_delivery_ack(frame: &Frame) -> bool {
    frame
        .envelope
        .headers
        .get(HEADER_AIRC_DELIVERY_ACK)
        .map(String::as_str)
        == Some(DELIVERY_ACK_REQUEST)
}

/// If this frame is an ack response, decode its [`DeliveryAck`] body.
/// Returns `None` for ordinary frames (no header / wrong value) and
/// for marked frames whose body does not parse — callers treat those
/// as ordinary frames rather than failing the ingest path.
pub fn decode_delivery_ack(frame: &Frame) -> Option<DeliveryAck> {
    if frame
        .envelope
        .headers
        .get(HEADER_AIRC_DELIVERY_ACK)
        .map(String::as_str)
        != Some(DELIVERY_ACK_RESPONSE)
    {
        return None;
    }
    let body = frame.envelope.body.as_ref()?;
    match body {
        Body::Json(value) => serde_json::from_value(value.clone()).ok(),
        Body::Binary(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, FrameKind};
    use crate::signature::Signature;
    use airc_core::{headers::Headers, transcript::MentionTarget, ClientId};

    fn frame_with_headers(headers: Headers, body: Option<Body>) -> Frame {
        Frame {
            kind: FrameKind::Control,
            envelope: Envelope {
                event_id: EventId::from_u128(0x1),
                sender: PeerId::from_u128(0xa1),
                sender_client: ClientId::from_u128(0xc1),
                channel: RoomId::from_u128(0xc0ffee),
                target: MentionTarget::All,
                lamport: 7,
                occurred_at_ms: 1_700_000_000_000,
                reply_to: None,
                headers,
                body,
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    // -----------------------------------------------------------------
    // Literal-JSON wire pins. These strings are the wire contract for
    // the new vocabulary: if a field or variant is renamed, these
    // tests fail BEFORE a cross-build peer silently fails to decode.
    // Mutation-verified during development: renaming `for_event` →
    // `for_event_id` and `unknown_channel` → `channel_unknown` each
    // broke the corresponding pin; restoring fixed it.
    // -----------------------------------------------------------------

    #[test]
    fn delivered_ack_wire_pin() {
        let ack = DeliveryAck {
            for_event: EventId::from_u128(0x1),
            receiver: PeerId::from_u128(0xb2),
            outcome: DeliveryOutcome::Delivered {
                channel: RoomId::from_u128(0xc0ffee),
                cursor: TranscriptCursor {
                    lamport: 42,
                    event_id: EventId::from_u128(0x1),
                },
            },
        };
        let expected = serde_json::json!({
            "for_event": "00000000-0000-0000-0000-000000000001",
            "receiver": "00000000-0000-0000-0000-0000000000b2",
            "outcome": {
                "status": "delivered",
                "channel": "00000000-0000-0000-0000-000000c0ffee",
                "cursor": {
                    "lamport": 42,
                    "event_id": "00000000-0000-0000-0000-000000000001"
                }
            }
        });
        let encoded = serde_json::to_value(&ack).expect("encode");
        assert_eq!(encoded, expected, "delivered ack wire shape is pinned");
        let decoded: DeliveryAck = serde_json::from_value(expected).expect("decode");
        assert_eq!(decoded, ack, "delivered ack decodes from pinned JSON");
    }

    #[test]
    fn undeliverable_ack_wire_pin() {
        let ack = DeliveryAck {
            for_event: EventId::from_u128(0x2),
            receiver: PeerId::from_u128(0xb2),
            outcome: DeliveryOutcome::Undeliverable {
                reason: UndeliverableReason::UnknownChannel,
            },
        };
        let expected = serde_json::json!({
            "for_event": "00000000-0000-0000-0000-000000000002",
            "receiver": "00000000-0000-0000-0000-0000000000b2",
            "outcome": {
                "status": "undeliverable",
                "reason": "unknown_channel"
            }
        });
        let encoded = serde_json::to_value(&ack).expect("encode");
        assert_eq!(encoded, expected, "undeliverable ack wire shape is pinned");
        let decoded: DeliveryAck = serde_json::from_value(expected).expect("decode");
        assert_eq!(decoded, ack, "undeliverable ack decodes from pinned JSON");
    }

    #[test]
    fn undeliverable_reason_labels_match_wire_encoding() {
        for reason in [
            UndeliverableReason::UnknownChannel,
            UndeliverableReason::PersistFailed,
            UndeliverableReason::NoSubscriber,
            UndeliverableReason::DecodeFailure,
            UndeliverableReason::VerificationFailed,
        ] {
            let wire = serde_json::to_value(reason).expect("encode reason");
            assert_eq!(
                wire,
                serde_json::Value::String(reason.as_str().to_string()),
                "as_str must equal the serde wire label for {reason:?}"
            );
        }
    }

    #[test]
    fn header_constants_are_pinned() {
        // These strings travel on the wire as header keys/values —
        // pinned so a rename is caught here, not in the field.
        assert_eq!(HEADER_AIRC_DELIVERY_ACK, "airc.delivery_ack");
        assert_eq!(DELIVERY_ACK_REQUEST, "request");
        assert_eq!(DELIVERY_ACK_RESPONSE, "response");
    }

    #[test]
    fn wants_delivery_ack_requires_exact_request_value() {
        let mut headers = Headers::new();
        headers.insert(
            HEADER_AIRC_DELIVERY_ACK.to_string(),
            DELIVERY_ACK_REQUEST.to_string(),
        );
        assert!(wants_delivery_ack(&frame_with_headers(headers, None)));

        assert!(!wants_delivery_ack(&frame_with_headers(
            Headers::new(),
            None
        )));

        let mut response_headers = Headers::new();
        response_headers.insert(
            HEADER_AIRC_DELIVERY_ACK.to_string(),
            DELIVERY_ACK_RESPONSE.to_string(),
        );
        assert!(!wants_delivery_ack(&frame_with_headers(
            response_headers,
            None
        )));
    }

    #[test]
    fn decode_delivery_ack_roundtrips_through_a_frame_body() {
        let ack = DeliveryAck {
            for_event: EventId::from_u128(0x3),
            receiver: PeerId::from_u128(0xb2),
            outcome: DeliveryOutcome::Undeliverable {
                reason: UndeliverableReason::PersistFailed,
            },
        };
        let mut headers = Headers::new();
        headers.insert(
            HEADER_AIRC_DELIVERY_ACK.to_string(),
            DELIVERY_ACK_RESPONSE.to_string(),
        );
        let frame = frame_with_headers(
            headers,
            Some(Body::Json(serde_json::to_value(&ack).expect("encode"))),
        );
        assert_eq!(decode_delivery_ack(&frame), Some(ack));

        // Ordinary frames (no marker header) never decode as acks even
        // if their body happens to look like one.
        let plain = frame_with_headers(Headers::new(), None);
        assert_eq!(decode_delivery_ack(&plain), None);
    }
}
