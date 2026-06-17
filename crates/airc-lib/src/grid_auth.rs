//! Grid auth — typed, signed grants over airc's cryptographic identity.
//!
//! The substrate already has the crypto root (Ed25519 signed identity),
//! the account fence (`MeshIdentity`), the typed grant (`TrustTier`),
//! and the capability vocabulary (`capability_tags`). The signed
//! identity IS the token. This module is the typed signed-grant layer
//! over that crypto — what continuum / Hermes / OpenClaw consume by lib
//! instead of reinventing auth.
//!
//! Design: `docs/architecture/GRID-AUTH-MODEL.md`.
//!
//! ## What this slice is (and is NOT)
//!
//! Slice 1 is the grant structs, a credential-agnostic verifier seam,
//! and the verification logic (issuer / expiry / capability checks),
//! proven with a stub verifier. The grant body is fixed; the proof layer
//! ([`GrantProof`]) is where credential paradigms (passkeys), multi-sig,
//! and forge-alloy Merkle anchors grow — so the body never changes.
//!
//! Deferred until the design's open questions are signed off: the real
//! ed25519 [`GrantVerifier`] impl, wiring into
//! `grid/acl.rs::is_command_authorized`, attestation issuance, and
//! issuer-key distribution. Those are security policy, not foundation.

use airc_core::PeerId;
use airc_store::peer_trust::TrustTier;
use serde::{Deserialize, Serialize};

use crate::subscriptions::MeshIdentity;

/// The credential paradigm a grant's signature uses.
///
/// Credential-agnostic by design — the WebAuthn / passkey extension
/// point. A new variant (e.g. `WebAuthn`) adds a credential paradigm
/// WITHOUT touching any grant body; only a new [`GrantVerifier`] impl is
/// needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialKind {
    /// Ed25519 — the airc identity keypair (the default grid credential).
    Ed25519,
    // WebAuthn — hardware-backed platform passkey (future slice).
}

/// The proof layer of a signed grant: who signed (issuer pubkey), under
/// which credential paradigm, and the signature bytes.
///
/// Separated from the grant BODY on purpose: credential paradigms
/// (passkeys), multi-signature thresholds, and Merkle anchoring all grow
/// HERE, leaving the body — and therefore every consumer that reads the
/// body — untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantProof {
    pub credential: CredentialKind,
    /// The issuer's public key (the account-owner key is the root of
    /// trust). Bytes so it is credential-agnostic.
    pub issuer_pubkey: Vec<u8>,
    /// Signature over the canonical bytes of the grant body.
    pub signature: Vec<u8>,
}

/// Verifies a signature over a message for a given [`GrantProof`]. One
/// impl per credential paradigm (ed25519 now; WebAuthn later). Pure — no
/// IO, no clock — so the grant logic that calls it stays deterministic
/// and unit-testable with a stub.
pub trait GrantVerifier {
    fn verify_signature(&self, message: &[u8], proof: &GrantProof) -> bool;
}

/// Attestation that `subject` belongs to a mesh identity (a GitHub
/// account = the owner's grid). The grant **body**; signed in
/// [`SignedMeshMembership`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshMembershipAttestation {
    pub subject: PeerId,
    /// Bind the attestation to the subject's KEY, not just its uuid, so
    /// a stolen peer_id can't ride someone else's attestation.
    pub subject_pubkey: Vec<u8>,
    pub mesh_identity: MeshIdentity,
    /// The default trust tier this membership confers. `OwnAccount` for a
    /// plain same-account member (the airc-trust tier meaning "same GH
    /// account, different machine"); the owner may attest higher.
    pub default_tier: TrustTier,
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,
}

/// A [`MeshMembershipAttestation`] + the owner's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMeshMembership {
    pub attestation: MeshMembershipAttestation,
    pub proof: GrantProof,
}

/// A typed capability grant — the explicit, signed delegation for
/// cross-account / external assistants (Hermes, OpenClaw) or grants
/// beyond a same-account member's default tier. The grant **body**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    pub grantee: PeerId,
    pub grantee_pubkey: Vec<u8>,
    /// Capability tags — the SAME vocabulary `CapabilityRegistry` matches
    /// on (e.g. `"ai/generate"`, `"inference/serve"`). Typed reuse, not
    /// a parallel string namespace.
    pub capabilities: Vec<String>,
    pub granted_in: MeshIdentity,
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    /// Monotonic per grantee — latest epoch wins. A revocation is a
    /// higher-epoch grant with empty `capabilities` (no separate
    /// revocation channel to keep in sync).
    pub epoch: u64,
}

/// A [`CapabilityGrant`] + the owner's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCapabilityGrant {
    pub grant: CapabilityGrant,
    pub proof: GrantProof,
}

/// Outcome of verifying a signed grant. A typed verdict (not a bool) so
/// callers + audit can see *why* a grant was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantVerdict {
    Valid,
    /// Signature did not verify for the proof's credential.
    BadSignature,
    /// The issuer pubkey is not the trusted account-owner key.
    UntrustedIssuer,
    /// `now_ms` is at/after `expires_at_ms`.
    Expired,
}

/// Deterministic bytes the signature is computed/verified over. serde_json
/// for slice 1; a canonical encoding (sorted keys / CBOR) is finalized
/// with the real ed25519 verifier slice, where it is load-bearing.
fn signing_bytes<T: Serialize>(body: &T) -> Vec<u8> {
    serde_json::to_vec(body).unwrap_or_default()
}

/// Shared verdict logic for any signed grant: trusted issuer, not
/// expired, signature valid. Generic over the body so both grant kinds
/// reuse exactly one decision path.
fn verify_signed<T: Serialize>(
    body: &T,
    proof: &GrantProof,
    now_ms: u64,
    expires_at_ms: Option<u64>,
    verifier: &dyn GrantVerifier,
    trusted_issuer_pubkey: &[u8],
) -> GrantVerdict {
    if proof.issuer_pubkey != trusted_issuer_pubkey {
        return GrantVerdict::UntrustedIssuer;
    }
    if let Some(exp) = expires_at_ms {
        if now_ms >= exp {
            return GrantVerdict::Expired;
        }
    }
    if !verifier.verify_signature(&signing_bytes(body), proof) {
        return GrantVerdict::BadSignature;
    }
    GrantVerdict::Valid
}

impl SignedMeshMembership {
    /// Verify this membership against the trusted account-owner key.
    /// `Valid` ⇒ the caller may grant `attestation.default_tier` to the
    /// subject (the `mesh_identity → TrustTier` bridge, no manual step).
    pub fn verify(
        &self,
        now_ms: u64,
        verifier: &dyn GrantVerifier,
        trusted_issuer_pubkey: &[u8],
    ) -> GrantVerdict {
        verify_signed(
            &self.attestation,
            &self.proof,
            now_ms,
            self.attestation.expires_at_ms,
            verifier,
            trusted_issuer_pubkey,
        )
    }
}

impl SignedCapabilityGrant {
    /// Verify this grant against the trusted account-owner key.
    pub fn verify(
        &self,
        now_ms: u64,
        verifier: &dyn GrantVerifier,
        trusted_issuer_pubkey: &[u8],
    ) -> GrantVerdict {
        verify_signed(
            &self.grant,
            &self.proof,
            now_ms,
            self.grant.expires_at_ms,
            verifier,
            trusted_issuer_pubkey,
        )
    }

    /// Whether this grant confers `capability` (a capability tag). Does
    /// NOT verify the signature — call [`Self::verify`] first.
    pub fn grants(&self, capability: &str) -> bool {
        self.grant.capabilities.iter().any(|c| c == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Stub verifier: returns a fixed result, so the grant LOGIC (issuer,
    /// expiry, capability) is tested without a real signature. The real
    /// ed25519 verifier is a later slice; this proves the seam + policy.
    struct StubVerifier {
        valid: bool,
    }
    impl GrantVerifier for StubVerifier {
        fn verify_signature(&self, _message: &[u8], _proof: &GrantProof) -> bool {
            self.valid
        }
    }

    fn peer(n: u128) -> PeerId {
        PeerId::from_uuid(Uuid::from_u128(n))
    }

    fn grant(caps: &[&str], expires_at_ms: Option<u64>, issuer: Vec<u8>) -> SignedCapabilityGrant {
        SignedCapabilityGrant {
            grant: CapabilityGrant {
                grantee: peer(2),
                grantee_pubkey: vec![2, 2, 2],
                capabilities: caps.iter().map(|c| c.to_string()).collect(),
                granted_in: MeshIdentity::new("joelteply"),
                issued_at_ms: 1_000,
                expires_at_ms,
                epoch: 1,
            },
            proof: GrantProof {
                credential: CredentialKind::Ed25519,
                issuer_pubkey: issuer,
                signature: vec![9, 9, 9],
            },
        }
    }

    const OWNER: &[u8] = &[1, 1, 1];

    // what this catches: a valid grant from the trusted owner, not
    // expired, with a good signature, verifies AND confers exactly its
    // capability tags.
    #[test]
    fn valid_grant_verifies_and_confers_capability() {
        let g = grant(&["ai/generate"], None, OWNER.to_vec());
        assert_eq!(
            g.verify(2_000, &StubVerifier { valid: true }, OWNER),
            GrantVerdict::Valid
        );
        assert!(g.grants("ai/generate"));
        assert!(!g.grants("data/delete"), "grant confers only its own tags");
    }

    // what this catches: a grant signed by a key that is NOT the trusted
    // owner is rejected BEFORE the signature is even checked — issuer
    // pinning is the first gate.
    #[test]
    fn untrusted_issuer_is_rejected() {
        let g = grant(&["ai/generate"], None, vec![7, 7, 7]); // not OWNER
        assert_eq!(
            g.verify(2_000, &StubVerifier { valid: true }, OWNER),
            GrantVerdict::UntrustedIssuer
        );
    }

    // what this catches: an expired grant is rejected even with a valid
    // signature from the trusted owner (now_ms >= expires_at_ms).
    #[test]
    fn expired_grant_is_rejected() {
        let g = grant(&["ai/generate"], Some(1_500), OWNER.to_vec());
        assert_eq!(
            g.verify(2_000, &StubVerifier { valid: true }, OWNER),
            GrantVerdict::Expired
        );
        // exactly at expiry is expired (>=).
        assert_eq!(
            g.verify(1_500, &StubVerifier { valid: true }, OWNER),
            GrantVerdict::Expired
        );
        // before expiry is fine.
        assert_eq!(
            g.verify(1_499, &StubVerifier { valid: true }, OWNER),
            GrantVerdict::Valid
        );
    }

    // what this catches: a bad signature is rejected (the credential
    // seam's verdict propagates) — even from the trusted issuer, unexpired.
    #[test]
    fn bad_signature_is_rejected() {
        let g = grant(&["ai/generate"], None, OWNER.to_vec());
        assert_eq!(
            g.verify(2_000, &StubVerifier { valid: false }, OWNER),
            GrantVerdict::BadSignature
        );
    }

    // what this catches: the same verdict path is reused for membership
    // attestations (the mesh_identity -> TrustTier bridge), so the
    // default_tier is only honored on a Valid verdict.
    #[test]
    fn mesh_membership_uses_the_same_verdict_path() {
        let m = SignedMeshMembership {
            attestation: MeshMembershipAttestation {
                subject: peer(3),
                subject_pubkey: vec![3, 3, 3],
                mesh_identity: MeshIdentity::new("joelteply"),
                default_tier: TrustTier::OwnAccount,
                issued_at_ms: 1_000,
                expires_at_ms: Some(5_000),
            },
            proof: GrantProof {
                credential: CredentialKind::Ed25519,
                issuer_pubkey: OWNER.to_vec(),
                signature: vec![9, 9, 9],
            },
        };
        assert_eq!(
            m.verify(2_000, &StubVerifier { valid: true }, OWNER),
            GrantVerdict::Valid
        );
        assert_eq!(
            m.verify(2_000, &StubVerifier { valid: true }, &[7, 7, 7]),
            GrantVerdict::UntrustedIssuer
        );
        assert_eq!(m.attestation.default_tier, TrustTier::OwnAccount);
    }
}
