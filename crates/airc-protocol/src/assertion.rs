//! Domain-separated identity assertions — the airc analogue of a
//! WebAuthn assertion.
//!
//! A participant signs over a versioned **domain tag** + a caller
//! `context` (the "type" / relying-party binding, like WebAuthn's
//! `clientDataJSON.type` + RP ID) + a `challenge` (server nonce or
//! payload). The domain tag makes this signature space provably
//! DISJOINT from envelope/frame signatures (which sign canonical CBOR,
//! never this ASCII prefix) and from trust-rotation bodies — so a
//! session token can never be replayed as a frame signature, or vice
//! versa (the confused-deputy hole WebAuthn closes with its `type`
//! field).
//!
//! Consumers (Continuum, jtag, browser/server clients) build session
//! tokens + credential bindings on top: **mint** = sign an assertion
//! over a session descriptor; **verify** = `verify` against the trust
//! registry the mesh already distributes (or `verify_with_pubkey` from
//! a peer-spec pubkey). The signer is reached only through the
//! identity's keypair — never the raw key — so a later device-bound /
//! Secure-Enclave backend is a drop-in (the assertion API is unchanged;
//! only where the signature comes from changes).

use ed25519_dalek::{Signature as Ed25519Sig, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::signature::PeerKeyRegistry;

/// Versioned domain tag prepended to every assertion's signed bytes.
/// Frame canonical CBOR begins with map/array markers (never this
/// ASCII), so assertion and frame signatures can't collide. Bump the
/// version to rotate the scheme.
pub const ASSERTION_DOMAIN: &[u8] = b"airc-identity-assertion:v1";

/// The exact bytes a participant signs: domain tag, then
/// length-delimited `context` and `challenge`. Length prefixes prevent
/// `(ctx="a", chal="bc")` from colliding with `(ctx="ab", chal="c")`.
pub(crate) fn assertion_signing_bytes(context: &str, challenge: &[u8]) -> Vec<u8> {
    let ctx = context.as_bytes();
    let mut out = Vec::with_capacity(ASSERTION_DOMAIN.len() + 8 + ctx.len() + challenge.len());
    out.extend_from_slice(ASSERTION_DOMAIN);
    out.extend_from_slice(&(ctx.len() as u32).to_le_bytes());
    out.extend_from_slice(ctx);
    out.extend_from_slice(&(challenge.len() as u32).to_le_bytes());
    out.extend_from_slice(challenge);
    out
}

/// A signed identity assertion: who signed (`peer_id` + `key_id` for
/// rotation), the `context` (RP/"type" binding), the `challenge`, and
/// the 64-byte Ed25519 signature. The verifier reconstructs the signed
/// bytes from `context` + `challenge` + the fixed domain tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityAssertion {
    pub peer_id: PeerId,
    pub key_id: u32,
    pub context: String,
    pub challenge: Vec<u8>,
    #[serde(with = "crate::signature::serde_bytes_64")]
    pub signature: [u8; 64],
}

/// Why verifying an assertion might fail. Fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssertionError {
    /// Signer is not enrolled in the registry (unknown `(peer, key_id)`).
    UnknownSigner(PeerId),
    /// The supplied public key bytes are not a valid Ed25519 point.
    InvalidPublicKey,
    /// Signature did not verify over `(domain, context, challenge)`.
    BadSignature,
}

impl std::fmt::Display for AssertionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssertionError::UnknownSigner(peer) => write!(f, "unknown assertion signer {peer}"),
            AssertionError::InvalidPublicKey => write!(f, "invalid assertion public key"),
            AssertionError::BadSignature => write!(f, "bad assertion signature"),
        }
    }
}

impl std::error::Error for AssertionError {}

impl IdentityAssertion {
    /// Verify against the trust registry the mesh distributes.
    pub fn verify(&self, registry: &PeerKeyRegistry) -> Result<(), AssertionError> {
        let key = registry
            .lookup(self.peer_id, self.key_id)
            .ok_or(AssertionError::UnknownSigner(self.peer_id))?;
        self.verify_with_key(&key)
    }

    /// Verify against a 32-byte Ed25519 public key (e.g. parsed from a
    /// peer-spec) — for consumers that hold the pubkey directly rather
    /// than a full registry.
    pub fn verify_with_pubkey(&self, pubkey: &[u8; 32]) -> Result<(), AssertionError> {
        let key = VerifyingKey::from_bytes(pubkey).map_err(|_| AssertionError::InvalidPublicKey)?;
        self.verify_with_key(&key)
    }

    fn verify_with_key(&self, key: &VerifyingKey) -> Result<(), AssertionError> {
        let bytes = assertion_signing_bytes(&self.context, &self.challenge);
        let sig = Ed25519Sig::from_bytes(&self.signature);
        key.verify(&bytes, &sig)
            .map_err(|_| AssertionError::BadSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::canonical_signed_bytes;
    use crate::envelope::{ChannelId, Envelope};
    use crate::keypair::PeerKeypair;
    use airc_core::{headers::Headers, transcript::MentionTarget, Body, ClientId, EventId};

    fn registry_with(peer: PeerId, kp: &PeerKeypair) -> PeerKeyRegistry {
        let registry = PeerKeyRegistry::new();
        registry.enrol(peer, 0, kp.public_bytes()).expect("enrol");
        registry
    }

    #[test]
    fn assertion_round_trips_and_verifies() {
        let kp = PeerKeypair::generate();
        let peer = PeerId::new();
        let registry = registry_with(peer, &kp);

        let assertion = kp.sign_assertion(peer, 0, "continuum.session", b"nonce-42");
        assert_eq!(assertion.verify(&registry), Ok(()));
        assert_eq!(assertion.verify_with_pubkey(&kp.public_bytes()), Ok(()));
    }

    #[test]
    fn tampered_context_challenge_or_sig_fails_closed() {
        let kp = PeerKeypair::generate();
        let peer = PeerId::new();
        let registry = registry_with(peer, &kp);
        let assertion = kp.sign_assertion(peer, 0, "continuum.session", b"nonce-42");

        let mut wrong_ctx = assertion.clone();
        wrong_ctx.context = "continuum.other".to_string();
        assert_eq!(wrong_ctx.verify(&registry), Err(AssertionError::BadSignature));

        let mut wrong_chal = assertion.clone();
        wrong_chal.challenge = b"nonce-43".to_vec();
        assert_eq!(
            wrong_chal.verify(&registry),
            Err(AssertionError::BadSignature)
        );

        let mut wrong_sig = assertion.clone();
        wrong_sig.signature[0] ^= 0xFF;
        assert_eq!(wrong_sig.verify(&registry), Err(AssertionError::BadSignature));
    }

    #[test]
    fn unknown_signer_fails_closed() {
        let kp = PeerKeypair::generate();
        let peer = PeerId::new();
        let empty = PeerKeyRegistry::new();
        let assertion = kp.sign_assertion(peer, 0, "continuum.session", b"x");
        assert_eq!(
            assertion.verify(&empty),
            Err(AssertionError::UnknownSigner(peer))
        );
    }

    /// The crux: a frame signature and an assertion signature are in
    /// disjoint domains. The assertion's signed bytes carry the ASCII
    /// domain tag; an envelope's canonical CBOR never starts with it —
    /// so neither can be replayed as the other (WebAuthn's `type`
    /// binding, made concrete).
    #[test]
    fn assertion_domain_is_disjoint_from_frame_canonical_bytes() {
        let assertion_bytes = assertion_signing_bytes("continuum.session", b"nonce");
        assert!(assertion_bytes.starts_with(ASSERTION_DOMAIN));

        let envelope = Envelope {
            event_id: EventId::from_u128(0x01),
            sender: PeerId::new(),
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from_u128(0xc0ffee),
            target: MentionTarget::All,
            lamport: 1,
            occurred_at_ms: 1,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text("hi")),
            media: Vec::new(),
            signature: crate::signature::Signature::Unsigned,
        };
        let frame_bytes = canonical_signed_bytes(&envelope).expect("canonical");
        assert!(
            !frame_bytes.starts_with(ASSERTION_DOMAIN),
            "frame canonical bytes must never collide with the assertion domain"
        );
    }
}
