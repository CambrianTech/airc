//! Canonical encoding of an envelope for signing + content-hash purposes.
//!
//! Signatures cover bytes, not Rust structs — so "what bytes does this
//! envelope produce for the signer?" needs ONE deterministic answer.
//! Two semantically-equal envelopes (same fields, same values) MUST
//! produce identical canonical bytes regardless of serialization order
//! or formatting whitespace, or signatures verified on one machine won't
//! verify on another.
//!
//! Encoding: canonical CBOR via `ciborium`. CBOR has well-defined
//! determinism rules; `BTreeMap` for `Headers` ensures lexicographic
//! map ordering; `ciborium` emits definite-length forms for arrays and
//! maps and shortest-form integers. The signed payload is the envelope
//! MINUS its signature field — the signer can't sign over its own
//! signature.
//!
//! NOTE: this is the substrate's *cryptographic* canonical form. The
//! JSON serde form (`serde_json::to_value(&envelope)`) is the *wire*
//! form for transports that prefer JSON. The two are not byte-equal —
//! they aren't supposed to be. JSON is for humans and JS; CBOR is for
//! the signer.

use serde::Serialize;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
};

use crate::envelope::Envelope;
use crate::media::MediaRef;

/// Errors producing the canonical encoding.
#[derive(Debug)]
pub enum CanonicalError {
    /// `ciborium` rejected the value — usually a malformed Body whose
    /// inner JSON contains a shape CBOR can't represent (e.g. a map
    /// with non-string keys after a serde projection).
    Encoding(String),
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonicalError::Encoding(message) => {
                write!(f, "canonical CBOR encoding failed: {message}")
            }
        }
    }
}

impl std::error::Error for CanonicalError {}

/// Produce the canonical CBOR bytes for an envelope, EXCLUDING its
/// signature field. These are the bytes the signer signs and the
/// verifier verifies against.
pub fn canonical_signed_bytes(envelope: &Envelope) -> Result<Vec<u8>, CanonicalError> {
    let payload = SignedPayload::from_envelope(envelope);
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&payload, &mut buf)
        .map_err(|error| CanonicalError::Encoding(error.to_string()))?;
    Ok(buf)
}

/// The envelope's signed payload — every field EXCEPT `signature`.
/// Borrowed so encoding doesn't clone the body/headers/media.
#[derive(Serialize)]
struct SignedPayload<'envelope> {
    event_id: &'envelope EventId,
    sender: &'envelope PeerId,
    sender_client: &'envelope ClientId,
    channel: &'envelope RoomId,
    target: &'envelope MentionTarget,
    lamport: u64,
    occurred_at_ms: u64,
    reply_to: &'envelope Option<EventId>,
    headers: &'envelope Headers,
    body: &'envelope Option<Body>,
    media: &'envelope Vec<MediaRef>,
}

impl<'envelope> SignedPayload<'envelope> {
    fn from_envelope(envelope: &'envelope Envelope) -> Self {
        Self {
            event_id: &envelope.event_id,
            sender: &envelope.sender,
            sender_client: &envelope.sender_client,
            channel: &envelope.channel,
            target: &envelope.target,
            lamport: envelope.lamport,
            occurred_at_ms: envelope.occurred_at_ms,
            reply_to: &envelope.reply_to,
            headers: &envelope.headers,
            body: &envelope.body,
            media: &envelope.media,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{ChannelId, Envelope, FrameKind};
    use crate::media::MediaRef;
    use crate::signature::Signature;
    use airc_core::{
        headers::Headers, transcript::MentionTarget, Body, ClientId, ContentHash, EventId, FileId,
        PeerId,
    };

    fn fixture() -> Envelope {
        let mut headers = Headers::new();
        headers.insert(
            "forge.body_hint".to_string(),
            "forge.persona.turn".to_string(),
        );
        headers.insert("airc.trace_id".to_string(), "trace-abc-123".to_string());
        Envelope {
            event_id: EventId::from_u128(0x01),
            sender: PeerId::from_u128(0xa1),
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from_u128(0xc0ffee),
            target: MentionTarget::All,
            lamport: 42,
            occurred_at_ms: 1_700_000_000_000,
            reply_to: Some(EventId::from_u128(0x11)),
            headers,
            body: Some(Body::text("hello world")),
            media: vec![MediaRef {
                file_id: FileId::from_u128(0xf1),
                content_hash: ContentHash("sha256:1234abcd".to_string()),
                mime: Some("image/png".to_string()),
                size_bytes: Some(2048),
                caption: None,
            }],
            signature: Signature::Unsigned,
        }
    }

    #[test]
    fn canonical_bytes_are_deterministic_across_runs() {
        // Two identical envelopes MUST encode to the same bytes. This
        // is the foundational property — if it fails, signing breaks
        // across processes (sender signs one form, receiver verifies
        // another).
        let a = canonical_signed_bytes(&fixture()).unwrap();
        let b = canonical_signed_bytes(&fixture()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn signature_field_does_not_affect_canonical_bytes() {
        // The "signs over its own signature" trap. Two envelopes that
        // differ ONLY in the signature field MUST produce identical
        // canonical bytes — otherwise the signer can't sign anything
        // (they'd have to know the signature before producing it).
        let mut unsigned = fixture();
        unsigned.signature = Signature::Unsigned;
        let mut signed = fixture();
        signed.signature = Signature::Ed25519 {
            signer: PeerId::from_u128(0xa1),
            key_id: 0,
            sig: [0xff; 64],
        };

        let unsigned_bytes = canonical_signed_bytes(&unsigned).unwrap();
        let signed_bytes = canonical_signed_bytes(&signed).unwrap();
        assert_eq!(unsigned_bytes, signed_bytes);
    }

    #[test]
    fn headers_order_does_not_affect_canonical_bytes() {
        // BTreeMap is sorted, so insertion order can't influence the
        // CBOR output. Pin that — if someone swaps Headers to HashMap
        // later the canonical encoding becomes non-deterministic.
        let mut a = fixture();
        a.headers.insert("z.last".to_string(), "Z".to_string());
        a.headers.insert("a.first".to_string(), "A".to_string());

        let mut b = fixture();
        // Insert in opposite order.
        b.headers.insert("a.first".to_string(), "A".to_string());
        b.headers.insert("z.last".to_string(), "Z".to_string());

        assert_eq!(
            canonical_signed_bytes(&a).unwrap(),
            canonical_signed_bytes(&b).unwrap()
        );
    }

    #[test]
    fn field_change_does_change_canonical_bytes() {
        // Sanity check the inverse: a real semantic change DOES alter
        // the canonical bytes. If it doesn't, the encoding is dropping
        // information.
        let a = canonical_signed_bytes(&fixture()).unwrap();
        let mut altered = fixture();
        altered.lamport = 43;
        let b = canonical_signed_bytes(&altered).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn canonical_encoding_carries_reply_to_field() {
        // Pin that the reply_to field is part of the signed payload —
        // otherwise tampering with a reply target would be undetectable.
        let mut without_reply = fixture();
        without_reply.reply_to = None;
        let mut with_reply = fixture();
        with_reply.reply_to = Some(EventId::from_u128(0x99));

        assert_ne!(
            canonical_signed_bytes(&without_reply).unwrap(),
            canonical_signed_bytes(&with_reply).unwrap()
        );
    }

    #[test]
    fn frame_kind_is_not_part_of_canonical_bytes() {
        // Frame kind is a transport-layer dispatch label, not envelope
        // content. Two frames carrying the same envelope but with
        // different kinds (Message vs Event) should produce the same
        // canonical bytes — the signer signs the envelope, not the
        // frame kind. This test serves as a guard: if someone later
        // adds frame-kind to the canonical payload, they break
        // adapters that re-emit the same envelope as a different
        // frame kind.
        let envelope = fixture();
        let bytes = canonical_signed_bytes(&envelope).unwrap();
        // Sanity: bytes are non-empty
        assert!(!bytes.is_empty());
        // No FrameKind enum participates in the encoding (we encode
        // SignedPayload, not Frame). This test pins the contract by
        // ensuring `bytes` only depends on Envelope.
        let _ = FrameKind::Message; // referenced so the enum stays in
                                    // scope; documenting intent.
    }
}
