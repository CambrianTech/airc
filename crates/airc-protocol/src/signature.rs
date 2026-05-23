//! Envelope signature + verification policy.
//!
//! Two-variant `Signature`: a real `Ed25519` shape (signer + key-id + 64
//! bytes) and an explicit `Unsigned` marker. There is **no silent-pass**
//! variant — production secure mode (`VerificationPolicy::Strict`) MUST
//! refuse unsigned frames. Dev mode (`VerificationPolicy::AllowUnsigned`)
//! permits `Unsigned` so the substrate is testable end-to-end before
//! every adapter is keyed.
//!
//! PR-3a wired real Ed25519 verification via `ed25519-dalek`. The verify
//! path:
//!   1. Structural validation (reply_to consistency).
//!   2. Policy dispatch on `Signature` variant.
//!   3. For `Ed25519`: registry lookup (UnknownSigner if missing),
//!      canonical CBOR encoding, byte-level signature verification.
//!
//! No silent passes, no "try secure then plaintext" fallback. Strict
//! policy fails closed on every failure mode.
//!
//! See `keypair` module for the send side (`PeerKeypair`).

use ed25519_dalek::{Signature as Ed25519Sig, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::canonical::canonical_signed_bytes;
use crate::envelope::{Envelope, Frame};
use crate::headers_keys::HEADER_AIRC_REPLY_TO;

/// Per-envelope signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Signature {
    /// Ed25519 signature over the canonical-CBOR encoding of the envelope
    /// (every field EXCEPT this one). The `signer` identifies which
    /// peer's pubkey is required to verify; `key_id` selects among that
    /// peer's enrolled keys (supports rotation).
    Ed25519 {
        signer: PeerId,
        key_id: u32,
        #[serde(with = "serde_bytes_64")]
        sig: [u8; 64],
    },

    /// Explicit "this frame is not signed." Only valid under
    /// `VerificationPolicy::AllowUnsigned`. Strict policy refuses.
    /// Adapters that haven't been wired with keypairs yet emit this
    /// in dev mode so end-to-end paths are exercisable.
    Unsigned,
}

/// Verification policy — what the substrate accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationPolicy {
    /// Production. Every frame must carry `Signature::Ed25519` with a
    /// recognised signer and a valid signature. `Unsigned` is refused.
    Strict,

    /// Development. `Unsigned` frames are accepted (the caller is
    /// expected to log them so the loosened policy is visible).
    /// `Ed25519` frames are still verified — relaxing strictness on
    /// unsigned does not mean accepting forged signatures.
    AllowUnsigned,
}

/// Why enroling a key might fail.
#[derive(Debug)]
pub enum KeyError {
    /// 32-byte pubkey didn't decode as a valid Ed25519 point. Caller
    /// passed garbage or a key from a different curve.
    InvalidPublicKey(ed25519_dalek::SignatureError),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::InvalidPublicKey(error) => {
                write!(f, "invalid Ed25519 public key: {error}")
            }
        }
    }
}

impl std::error::Error for KeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KeyError::InvalidPublicKey(error) => Some(error),
        }
    }
}

/// Registry of which `PeerId`s have which public keys.
///
/// Storage is parsed `VerifyingKey` rather than raw bytes, so verify
/// doesn't re-parse on every frame (per-frame parsing would burn a
/// noticeable fraction of the verify budget). `enrol` validates the
/// bytes once at enrolment time.
#[derive(Debug, Default, Clone)]
pub struct PeerKeyRegistry {
    // HashMap rather than BTreeMap — PeerId is a UUIDv4 newtype which
    // derives Hash+Eq but not Ord. The registry isn't serialized or
    // iterated for canonical output, so non-deterministic key order is
    // harmless here.
    keys: std::collections::HashMap<(PeerId, u32), VerifyingKey>,
}

impl PeerKeyRegistry {
    pub fn new() -> Self {
        Self {
            keys: std::collections::HashMap::new(),
        }
    }

    /// Enrol a peer's public key (32 bytes for Ed25519). `key_id`
    /// allows the same peer to have multiple keys for rotation.
    /// Returns `KeyError::InvalidPublicKey` if the bytes don't
    /// decode as a valid Ed25519 point — substrate refuses garbage
    /// at enrolment rather than at verify time.
    pub fn enrol(&mut self, peer: PeerId, key_id: u32, pubkey: [u8; 32]) -> Result<(), KeyError> {
        let verifying_key =
            VerifyingKey::from_bytes(&pubkey).map_err(KeyError::InvalidPublicKey)?;
        self.keys.insert((peer, key_id), verifying_key);
        Ok(())
    }

    /// Look up a parsed `VerifyingKey` for verification.
    pub fn lookup(&self, peer: PeerId, key_id: u32) -> Option<&VerifyingKey> {
        self.keys.get(&(peer, key_id))
    }

    /// Remove every enrolled key for `peer`.
    ///
    /// Used when local trust is explicitly revoked. Returns the
    /// number of key ids removed so callers can distinguish an
    /// idempotent remove from a real departure.
    pub fn remove_peer(&mut self, peer: PeerId) -> usize {
        let before = self.keys.len();
        self.keys
            .retain(|(enrolled_peer, _key_id), _key| *enrolled_peer != peer);
        before - self.keys.len()
    }

    /// Reverse lookup: find which `(peer, key_id)` enrolled a given
    /// pubkey. Used by the lan-tcp TLS verifier — at handshake time
    /// the server receives a client cert but doesn't know which peer
    /// is connecting; it extracts the cert's Ed25519 pubkey, calls
    /// `find_peer`, and either binds the connection to the resulting
    /// peer or rejects on miss.
    ///
    /// Returns the first match (each pubkey should be unique per
    /// `(peer, key_id)`, but if a duplicate is enrolled, the first
    /// hit wins). Iteration order is non-deterministic — fine for
    /// reverse-lookup correctness because we only need to know
    /// whether ANY match exists.
    pub fn find_peer(&self, pubkey: &[u8; 32]) -> Option<(PeerId, u32)> {
        self.keys.iter().find_map(|((peer, key_id), key)| {
            if key.as_bytes() == pubkey {
                Some((*peer, *key_id))
            } else {
                None
            }
        })
    }
}

/// What can go wrong when verifying a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationError {
    /// `Signature::Unsigned` submitted under `Strict` policy.
    MissingSignature,

    /// `Signature::Ed25519` signer is not in the registry.
    UnknownSigner(PeerId),

    /// Cryptographic verification failed — the canonical bytes did
    /// not verify against the enrolled `VerifyingKey`. Indicates
    /// tampering, replay with mutated content, or use of the wrong
    /// key. PR-3a wired this to real Ed25519; PR-1 had it stubbed.
    BadSignature,

    /// `Envelope.reply_to` and `headers["airc.reply_to"]` are both set
    /// but disagree. Adapters that project the structured field into
    /// the header must keep them in sync; receivers refuse the
    /// mismatch rather than picking one arbitrarily.
    ReplyToMismatch {
        structured: airc_core::EventId,
        header: String,
    },

    /// Canonical CBOR encoding of the signed payload could not be
    /// produced. Indicates a malformed envelope — usually a serde
    /// implementation that emits a non-CBOR-encodable value.
    CanonicalEncodingFailed,
}

impl std::fmt::Display for VerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerificationError::MissingSignature => {
                write!(f, "frame is unsigned and policy is Strict")
            }
            VerificationError::UnknownSigner(peer) => {
                write!(f, "signer {peer} is not in the peer key registry")
            }
            VerificationError::BadSignature => write!(f, "cryptographic signature did not verify"),
            VerificationError::ReplyToMismatch { structured, header } => write!(
                f,
                "envelope.reply_to ({structured}) disagrees with headers[airc.reply_to] ({header})"
            ),
            VerificationError::CanonicalEncodingFailed => {
                write!(f, "could not produce canonical CBOR encoding of envelope")
            }
        }
    }
}

impl std::error::Error for VerificationError {}

/// Verify a frame end-to-end: structural validation (reply_to consistency)
/// THEN policy + signature.
///
/// Adapters call this on every inbound frame. Strict policy fails closed
/// on any of: unsigned, unknown signer, bad signature, structural mismatch.
pub fn verify(
    frame: &Frame,
    policy: VerificationPolicy,
    registry: &PeerKeyRegistry,
) -> Result<(), VerificationError> {
    // Step 1: structural validation. Cheaper than crypto, and a mismatch
    // here means *something* is wrong even if the bytes happen to verify.
    check_reply_to_consistency(&frame.envelope)?;

    // Step 2: signature dispatch.
    match &frame.envelope.signature {
        Signature::Unsigned => match policy {
            VerificationPolicy::Strict => Err(VerificationError::MissingSignature),
            VerificationPolicy::AllowUnsigned => Ok(()),
        },
        Signature::Ed25519 {
            signer,
            key_id,
            sig,
        } => verify_ed25519(&frame.envelope, *signer, *key_id, sig, registry),
    }
}

/// Crypto-level verification for an Ed25519-signed envelope. Splits
/// out the lookup + canonical-encode + ed25519 verify steps so the
/// dispatcher in `verify` stays a clean policy match.
///
/// Steps:
///   1. Look up `(signer, key_id)` in the registry — `None` →
///      `UnknownSigner` (fail-closed).
///   2. Canonical-encode the envelope (everything except signature).
///   3. Ed25519-verify the 64-byte signature over those bytes.
fn verify_ed25519(
    envelope: &Envelope,
    signer: PeerId,
    key_id: u32,
    sig_bytes: &[u8; 64],
    registry: &PeerKeyRegistry,
) -> Result<(), VerificationError> {
    let verifying_key = registry
        .lookup(signer, key_id)
        .ok_or(VerificationError::UnknownSigner(signer))?;

    let canonical =
        canonical_signed_bytes(envelope).map_err(|_| VerificationError::CanonicalEncodingFailed)?;

    // `Ed25519Sig::from_bytes` cannot fail for a 64-byte array (the
    // newer dalek API is infallible-by-length). Any "this is not a
    // valid signature" comes out of `verify` itself.
    let signature = Ed25519Sig::from_bytes(sig_bytes);

    verifying_key
        .verify(&canonical, &signature)
        .map_err(|_| VerificationError::BadSignature)
}

/// Internal: enforce that the structured `reply_to` and the optional
/// header projection agree.
fn check_reply_to_consistency(envelope: &Envelope) -> Result<(), VerificationError> {
    let header = envelope.headers.get(HEADER_AIRC_REPLY_TO);
    match (envelope.reply_to, header) {
        (None, None) | (None, Some(_)) | (Some(_), None) => Ok(()),
        (Some(structured), Some(header_value)) => {
            // Authoritative form is the structured `EventId`. The header
            // carries the canonical UUID string. Equality is on the
            // string form of `EventId`, which is the hyphenated lowercase
            // produced by `Uuid::fmt`.
            let expected = structured.to_string();
            if expected == *header_value {
                Ok(())
            } else {
                Err(VerificationError::ReplyToMismatch {
                    structured,
                    header: header_value.clone(),
                })
            }
        }
    }
}

/// serde adapter for `[u8; 64]` — serde doesn't derive for arrays > 32.
pub(crate) mod serde_bytes_64 {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        let bytes = <Vec<u8> as Deserialize>::deserialize(deserializer)?;
        if bytes.len() != 64 {
            return Err(D::Error::custom(format!(
                "Ed25519 signature must be 64 bytes, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{ChannelId, Envelope, Frame, FrameKind};
    use airc_core::{headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId};

    /// Test helper — minimal valid envelope with a settable signature.
    /// All ids deterministic so failure cases are stable.
    fn envelope_with(sig: Signature) -> Envelope {
        Envelope {
            event_id: EventId::from_u128(0x01),
            sender: PeerId::from_u128(0xa1),
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from_u128(0xc0ffee),
            target: MentionTarget::All,
            lamport: 1,
            occurred_at_ms: 1_700_000_000_000,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text("hello")),
            media: Vec::new(),
            signature: sig,
        }
    }

    fn frame_with(sig: Signature) -> Frame {
        Frame {
            kind: FrameKind::Message,
            envelope: envelope_with(sig),
        }
    }

    #[test]
    fn unsigned_under_strict_fails_with_missing_signature() {
        // The canonical "fail closed in production" case. An adapter
        // that hasn't been keyed yet emits Unsigned; Strict refuses.
        let frame = frame_with(Signature::Unsigned);
        let registry = PeerKeyRegistry::new();
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::MissingSignature)
        );
    }

    #[test]
    fn unsigned_under_allow_unsigned_passes() {
        // The dev / bring-up path. Substrate works end-to-end before
        // keypairs are plumbed; loosened policy is the explicit opt-in.
        let frame = frame_with(Signature::Unsigned);
        let registry = PeerKeyRegistry::new();
        assert!(verify(&frame, VerificationPolicy::AllowUnsigned, &registry).is_ok());
    }

    #[test]
    fn ed25519_under_strict_with_unknown_signer_fails_closed() {
        // Ed25519 frame submitted but the peer isn't in the registry.
        // This is the path PR-1 exercises to prove fail-closed works
        // without needing real crypto plumbing.
        let signer = PeerId::from_u128(0xa1);
        let frame = frame_with(Signature::Ed25519 {
            signer,
            key_id: 0,
            sig: [0u8; 64],
        });
        let registry = PeerKeyRegistry::new();
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::UnknownSigner(signer))
        );
    }

    #[test]
    fn ed25519_under_allow_unsigned_with_unknown_signer_still_fails() {
        // AllowUnsigned does NOT mean "accept forged signatures." It
        // only relaxes the unsigned case. Ed25519 frames are still
        // verified against the registry.
        let signer = PeerId::from_u128(0xa1);
        let frame = frame_with(Signature::Ed25519 {
            signer,
            key_id: 0,
            sig: [0u8; 64],
        });
        let registry = PeerKeyRegistry::new();
        assert_eq!(
            verify(&frame, VerificationPolicy::AllowUnsigned, &registry),
            Err(VerificationError::UnknownSigner(signer))
        );
    }

    #[test]
    fn ed25519_with_garbage_signature_fails_bad_signature() {
        // PR-3a wired real Ed25519. An all-zero 64-byte signature
        // can't possibly verify against a real enrolled pubkey —
        // pin BadSignature as the resulting error. (PR-1 had a stub
        // that early-returned Ok here; this test replaces that stub
        // assertion with the correct fail-closed behavior.)
        use crate::keypair::PeerKeypair;
        let signer = PeerId::from_u128(0xa1);
        let real_keypair = PeerKeypair::generate();
        let mut registry = PeerKeyRegistry::new();
        registry
            .enrol(signer, 0, real_keypair.public_bytes())
            .unwrap();
        let frame = frame_with(Signature::Ed25519 {
            signer,
            key_id: 0,
            sig: [0u8; 64],
        });
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::BadSignature)
        );
    }

    #[test]
    fn reply_to_mismatch_fails_before_signature_check() {
        // Structural validation runs first. Even with Unsigned +
        // AllowUnsigned (the most lenient combo), a mismatch between
        // the structured reply_to and the header projection fails.
        let mut envelope = envelope_with(Signature::Unsigned);
        let structured = EventId::from_u128(0x42);
        envelope.reply_to = Some(structured);
        envelope.headers.insert(
            HEADER_AIRC_REPLY_TO.to_string(),
            EventId::from_u128(0x99).to_string(),
        );
        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        let registry = PeerKeyRegistry::new();
        let result = verify(&frame, VerificationPolicy::AllowUnsigned, &registry);
        match result {
            Err(VerificationError::ReplyToMismatch {
                structured: s,
                header: h,
            }) => {
                assert_eq!(s, structured);
                assert_eq!(h, EventId::from_u128(0x99).to_string());
            }
            other => panic!("expected ReplyToMismatch, got {other:?}"),
        }
    }

    #[test]
    fn reply_to_agreement_passes_validation() {
        // The "adapter projected the field into the header correctly"
        // case — common when an extension layers headers on top of an
        // already-structured envelope.
        let mut envelope = envelope_with(Signature::Unsigned);
        let reply = EventId::from_u128(0x42);
        envelope.reply_to = Some(reply);
        envelope
            .headers
            .insert(HEADER_AIRC_REPLY_TO.to_string(), reply.to_string());
        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        let registry = PeerKeyRegistry::new();
        assert!(verify(&frame, VerificationPolicy::AllowUnsigned, &registry).is_ok());
    }

    #[test]
    fn reply_to_field_only_or_header_only_is_fine() {
        // Asymmetric population is valid: an adapter that knows the
        // structured field may not bother emitting the header
        // projection (and vice versa for header-only adapters).
        let mut field_only = envelope_with(Signature::Unsigned);
        field_only.reply_to = Some(EventId::from_u128(0x42));
        assert!(verify(
            &Frame {
                kind: FrameKind::Message,
                envelope: field_only,
            },
            VerificationPolicy::AllowUnsigned,
            &PeerKeyRegistry::new(),
        )
        .is_ok());

        let mut header_only = envelope_with(Signature::Unsigned);
        header_only.headers.insert(
            HEADER_AIRC_REPLY_TO.to_string(),
            EventId::from_u128(0x42).to_string(),
        );
        assert!(verify(
            &Frame {
                kind: FrameKind::Message,
                envelope: header_only,
            },
            VerificationPolicy::AllowUnsigned,
            &PeerKeyRegistry::new(),
        )
        .is_ok());
    }

    #[test]
    fn remove_peer_removes_all_key_ids_for_peer() {
        use crate::keypair::PeerKeypair;

        let peer = PeerId::from_u128(0xabc);
        let other = PeerId::from_u128(0xdef);
        let key_a = PeerKeypair::generate();
        let key_b = PeerKeypair::generate();
        let key_other = PeerKeypair::generate();
        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, key_a.public_bytes()).unwrap();
        registry.enrol(peer, 1, key_b.public_bytes()).unwrap();
        registry.enrol(other, 0, key_other.public_bytes()).unwrap();

        assert_eq!(registry.remove_peer(peer), 2);
        assert!(registry.lookup(peer, 0).is_none());
        assert!(registry.lookup(peer, 1).is_none());
        assert!(registry.lookup(other, 0).is_some());
        assert_eq!(registry.remove_peer(peer), 0);
    }
}
