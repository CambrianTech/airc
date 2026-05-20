//! Peer keypair — sign envelopes with Ed25519.
//!
//! The send-side companion to `signature::verify`. A peer holds a
//! `PeerKeypair` (their private Ed25519 signing key) and uses it to
//! sign every outgoing envelope; receivers verify against the peer's
//! enrolled `VerifyingKey` in their `PeerKeyRegistry`.
//!
//! Crypto choice: Ed25519 via `ed25519-dalek`. Picked for the standard
//! reasons — small key + signature, deterministic signing (same input
//! always produces same signature, simplifies replay-testing), fast
//! verify (sub-50µs on modern silicon, fits the per-frame
//! verification budget), and broad ecosystem support (TLS 1.3 cert
//! identity, SSH, modern signature ecosystems all converge here).
//!
//! Storage: this module holds key material in memory only. Persistent
//! storage (SQLCipher at-rest, hardware-backed enclaves, etc.) is the
//! integrator's responsibility — `secret_bytes()` returns the raw 32
//! bytes for those integrations to handle. Substrate doesn't decide
//! where keys live; it only signs and verifies with them.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::{rngs::OsRng, RngCore};

use airc_core::PeerId;

use crate::canonical::{canonical_signed_bytes, CanonicalError};
use crate::envelope::Envelope;
use crate::signature::Signature;

/// A peer's private signing key + derived public key.
///
/// Construct with `generate()` (fresh random key) or
/// `from_secret_bytes(...)` (restore from persisted material). The
/// underlying `SigningKey` is held in memory only; integrators
/// arrange persistent storage outside the substrate.
#[derive(Clone)]
pub struct PeerKeypair {
    signing: SigningKey,
}

impl std::fmt::Debug for PeerKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never debug-print the secret. Pubkey is fine — it's public
        // by definition and useful for diagnostics.
        f.debug_struct("PeerKeypair")
            .field("public_bytes", &self.public_bytes())
            .field("secret_bytes", &"<redacted>")
            .finish()
    }
}

impl PeerKeypair {
    /// Generate a fresh random keypair from the OS CSPRNG.
    ///
    /// ed25519-dalek 2.x removed the convenience `SigningKey::generate`
    /// associated function; the substrate-style equivalent is to draw
    /// 32 bytes from `OsRng` and feed them through `from_bytes`.
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        Self {
            signing: SigningKey::from_bytes(&secret),
        }
    }

    /// Restore a keypair from previously-persisted 32 secret bytes.
    /// Use this to load a key from storage (SQLCipher, keychain, etc.).
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(secret),
        }
    }

    /// The 32-byte secret. Hand this to your at-rest storage layer.
    /// Treat with the discipline you'd apply to any other root secret.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The 32-byte public verifying key. Other peers enrol this in
    /// their `PeerKeyRegistry` so they can verify your signatures.
    pub fn public_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// The parsed `VerifyingKey` — convenience for direct registry
    /// enrolment without round-tripping through bytes.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// Raw Ed25519 signature over arbitrary bytes. Used by typed
    /// signed-blob features (e.g. `trust_rotation::sign_rotation`)
    /// that canonical-encode their own body and need a primitive
    /// signing op. The 64-byte sig matches the Ed25519 contract.
    pub fn sign_bytes(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.signing.sign(msg).to_bytes()
    }

    /// Sign an envelope: produces a `Signature::Ed25519` carrying the
    /// signer's PeerId, key_id (for rotation), and 64-byte signature
    /// over the envelope's canonical CBOR encoding.
    ///
    /// The signature covers every envelope field EXCEPT the
    /// signature itself (see `canonical::canonical_signed_bytes`).
    pub fn sign_envelope(
        &self,
        envelope: &Envelope,
        signer: PeerId,
        key_id: u32,
    ) -> Result<Signature, CanonicalError> {
        let bytes = canonical_signed_bytes(envelope)?;
        let sig = self.signing.sign(&bytes);
        Ok(Signature::Ed25519 {
            signer,
            key_id,
            sig: sig.to_bytes(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Frame;
    use crate::envelope::{ChannelId, Envelope, FrameKind};
    use crate::media::MediaRef;
    use crate::signature::{verify, PeerKeyRegistry, VerificationError, VerificationPolicy};
    use airc_core::{
        headers::Headers, transcript::MentionTarget, Body, ClientId, ContentHash, EventId, FileId,
        PeerId,
    };

    fn envelope_fixture(peer: PeerId) -> Envelope {
        let mut headers = Headers::new();
        headers.insert(
            "forge.body_hint".to_string(),
            "forge.persona.turn".to_string(),
        );
        Envelope {
            event_id: EventId::from_u128(0x01),
            sender: peer,
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from_u128(0xc0ffee),
            target: MentionTarget::All,
            lamport: 7,
            occurred_at_ms: 1_700_000_000_000,
            reply_to: None,
            headers,
            body: Some(Body::text("hello signed world")),
            media: vec![MediaRef {
                file_id: FileId::from_u128(0xf1),
                content_hash: ContentHash("sha256:abcd".to_string()),
                mime: Some("image/png".to_string()),
                size_bytes: Some(2048),
                caption: None,
            }],
            signature: Signature::Unsigned,
        }
    }

    #[test]
    fn sign_then_verify_with_enrolled_key_passes_under_strict() {
        // The base case: a peer signs an envelope, a receiver with
        // that peer's pubkey enrolled accepts under Strict policy.
        // This is the path Strict mode is built for.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut envelope = envelope_fixture(peer);
        envelope.signature = keypair.sign_envelope(&envelope, peer, 0).unwrap();

        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, keypair.public_bytes()).unwrap();

        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        assert!(verify(&frame, VerificationPolicy::Strict, &registry).is_ok());
    }

    #[test]
    fn tampered_envelope_after_signing_fails_bad_signature() {
        // The threat model the signature exists for. If anyone in the
        // middle alters any signed field, verify must reject.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut envelope = envelope_fixture(peer);
        envelope.signature = keypair.sign_envelope(&envelope, peer, 0).unwrap();

        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, keypair.public_bytes()).unwrap();

        // Tamper with the body — substantive content change.
        envelope.body = Some(Body::text("THIS IS NOT WHAT WAS SIGNED"));

        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::BadSignature)
        );
    }

    #[test]
    fn tampered_lamport_fails_bad_signature() {
        // Lamport is part of the canonical signed payload — tampering
        // with it must surface as BadSignature. Pins that ordering
        // fields are protected from replay-style manipulation.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut envelope = envelope_fixture(peer);
        envelope.signature = keypair.sign_envelope(&envelope, peer, 0).unwrap();
        envelope.lamport = 999;

        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, keypair.public_bytes()).unwrap();

        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::BadSignature)
        );
    }

    #[test]
    fn tampered_headers_fail_bad_signature() {
        // Header tampering — even adding a new header that wasn't
        // present at sign time must invalidate.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut envelope = envelope_fixture(peer);
        envelope.signature = keypair.sign_envelope(&envelope, peer, 0).unwrap();

        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, keypair.public_bytes()).unwrap();

        envelope
            .headers
            .insert("x-malicious".to_string(), "injected".to_string());

        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::BadSignature)
        );
    }

    #[test]
    fn tampered_reply_to_fails_bad_signature() {
        // reply_to is part of the signed payload. Cross-checking with
        // the structured-vs-header consistency check (PR-1) — both
        // layers refuse mismatches.
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut envelope = envelope_fixture(peer);
        envelope.signature = keypair.sign_envelope(&envelope, peer, 0).unwrap();

        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, keypair.public_bytes()).unwrap();

        envelope.reply_to = Some(EventId::from_u128(0x99));

        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::BadSignature)
        );
    }

    #[test]
    fn wrong_pubkey_in_registry_fails_bad_signature() {
        // The "evil twin" case: the registry has a key for this
        // peer, but not the right one. Verify must reject — this is
        // where the cryptographic check earns its keep.
        let peer = PeerId::from_u128(0xa1);
        let real_keypair = PeerKeypair::generate();
        let imposter_keypair = PeerKeypair::generate();
        let mut envelope = envelope_fixture(peer);
        envelope.signature = real_keypair.sign_envelope(&envelope, peer, 0).unwrap();

        let mut registry = PeerKeyRegistry::new();
        registry
            .enrol(peer, 0, imposter_keypair.public_bytes())
            .unwrap();

        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::BadSignature)
        );
    }

    #[test]
    fn unenrolled_signer_fails_unknown_signer() {
        // Empty registry → fail-closed with UnknownSigner. Pinned
        // again here because PR-3a wires real crypto on top — make
        // sure the error type still distinguishes "unknown signer"
        // from "signature failed."
        let peer = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut envelope = envelope_fixture(peer);
        envelope.signature = keypair.sign_envelope(&envelope, peer, 0).unwrap();

        let registry = PeerKeyRegistry::new();
        let frame = Frame {
            kind: FrameKind::Message,
            envelope,
        };
        assert_eq!(
            verify(&frame, VerificationPolicy::Strict, &registry),
            Err(VerificationError::UnknownSigner(peer))
        );
    }

    #[test]
    fn key_rotation_via_key_id_works() {
        // Same peer, two enrolled keys with different key_ids — both
        // can sign and both verify. This is how rotation works:
        // enrol the new key under a new key_id BEFORE retiring the
        // old one, then transition senders.
        let peer = PeerId::from_u128(0xa1);
        let old_keypair = PeerKeypair::generate();
        let new_keypair = PeerKeypair::generate();

        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, old_keypair.public_bytes()).unwrap();
        registry.enrol(peer, 1, new_keypair.public_bytes()).unwrap();

        let mut env_old = envelope_fixture(peer);
        env_old.signature = old_keypair.sign_envelope(&env_old, peer, 0).unwrap();
        let frame_old = Frame {
            kind: FrameKind::Message,
            envelope: env_old,
        };
        assert!(verify(&frame_old, VerificationPolicy::Strict, &registry).is_ok());

        let mut env_new = envelope_fixture(peer);
        env_new.signature = new_keypair.sign_envelope(&env_new, peer, 1).unwrap();
        let frame_new = Frame {
            kind: FrameKind::Message,
            envelope: env_new,
        };
        assert!(verify(&frame_new, VerificationPolicy::Strict, &registry).is_ok());
    }

    #[test]
    fn keypair_secret_roundtrips_through_bytes() {
        // Pin the persistence contract: secret_bytes -> from_secret_bytes
        // must yield a keypair that signs identically. Otherwise the
        // at-rest storage path is broken silently.
        let original = PeerKeypair::generate();
        let secret = original.secret_bytes();
        let restored = PeerKeypair::from_secret_bytes(&secret);

        let peer = PeerId::from_u128(0xa1);
        let envelope = envelope_fixture(peer);

        let sig_a = original.sign_envelope(&envelope, peer, 0).unwrap();
        let sig_b = restored.sign_envelope(&envelope, peer, 0).unwrap();
        // Ed25519 is deterministic — same key + same input = same sig.
        assert_eq!(sig_a, sig_b);
    }

    #[test]
    fn debug_redacts_secret() {
        // Pin that Debug never leaks the secret. If someone adds a
        // derive(Debug) on PeerKeypair in a refactor, this catches it.
        let kp = PeerKeypair::generate();
        let debug_string = format!("{kp:?}");
        assert!(
            debug_string.contains("<redacted>"),
            "Debug output must redact the secret; got: {debug_string}"
        );
        // Sanity: the actual 32-byte secret value should not appear
        // anywhere in the debug output.
        let secret = kp.secret_bytes();
        let secret_hex: String = secret.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(
            !debug_string.contains(&secret_hex),
            "Debug must not contain the secret hex"
        );
    }
}
