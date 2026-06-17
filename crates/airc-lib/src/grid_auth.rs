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
    ///
    /// DEFER (consumer integration slice; #1224 review finding 4):
    /// epoch/revocation is CONSUMER-SIDE state — [`SignedCapabilityGrant::verify`]
    /// does NOT track max-epoch-per-grantee. The consumer MUST persist the
    /// latest epoch it has accepted per grantee and reject a grant whose
    /// `epoch` is lower (a replayed/superseded grant). The verifier is
    /// stateless by design; the anti-replay state lives with the consumer.
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
    /// The issuer pubkey is not the trusted account-owner key.
    UntrustedIssuer,
    /// Signature did not verify for the proof's credential.
    BadSignature,
    /// The presenting peer's key is not the subject/grantee the grant is
    /// bound to — a stolen grant cannot ride another peer's identity.
    KeyMismatch,
    /// The grant is for a different mesh than the verifier's own grid.
    WrongMesh,
    /// `now_ms` is at/after `expires_at_ms`.
    Expired,
    /// The body could not be serialized to its canonical signed bytes —
    /// fail-CLOSED (we never verify a signature over empty bytes).
    EncodingFailed,
}

/// What a verifier checks a grant AGAINST: its own clock, the key of the
/// peer presenting the grant, its own mesh, the credential verifier, and
/// the trusted account-owner key. Bundling these keeps the two `verify`
/// entry points honest — a consumer cannot forget to pass the presenting
/// key or the expected mesh (the consumer-traps #1224 review flagged).
pub struct VerifyContext<'a> {
    pub now_ms: u64,
    /// The public key of the peer PRESENTING this grant (e.g. the airc
    /// peer making the request). Cross-checked against the grant's bound
    /// subject/grantee key.
    pub presenting_pubkey: &'a [u8],
    /// The verifier's OWN mesh identity. The grant must be scoped to it.
    pub expected_mesh: &'a MeshIdentity,
    pub verifier: &'a dyn GrantVerifier,
    /// The trusted account-owner key (the single root of trust).
    pub trusted_issuer_pubkey: &'a [u8],
}

/// Deterministic bytes the signature is computed/verified over. `None`
/// on a serialization failure — the caller treats that as a REJECT
/// (fail-closed), NEVER a verify over empty bytes (which would be
/// fail-open). serde_json for slice 1; a canonical encoding (sorted keys
/// / CBOR) is finalized with the real ed25519 verifier slice.
///
/// SIGNED-BYTE INVARIANT: the signature is over the serde_json of the
/// body, whose field order == struct declaration order. NEVER reorder
/// the fields of a signed body ([`CapabilityGrant`] /
/// [`MeshMembershipAttestation`]) — it changes the signed bytes and
/// invalidates every existing signature.
fn signing_bytes<T: Serialize>(body: &T) -> Option<Vec<u8>> {
    serde_json::to_vec(body).ok()
}

/// The ONE decision path for any signed grant. Order is
/// issuer → signature → key-binding → mesh → expiry: issuer-pin first
/// (so a wrong-issuer grant reports `UntrustedIssuer` for audit, not
/// `BadSignature`); signature before any body field is trusted; then the
/// bindings the #1224 review flagged as consumer-traps are enforced HERE
/// so a consumer cannot skip them.
fn verify_signed<T: Serialize>(
    body: &T,
    proof: &GrantProof,
    bound_pubkey: &[u8],
    body_mesh: &MeshIdentity,
    expires_at_ms: Option<u64>,
    ctx: &VerifyContext<'_>,
) -> GrantVerdict {
    if proof.issuer_pubkey != ctx.trusted_issuer_pubkey {
        return GrantVerdict::UntrustedIssuer;
    }
    let Some(bytes) = signing_bytes(body) else {
        return GrantVerdict::EncodingFailed;
    };
    if !ctx.verifier.verify_signature(&bytes, proof) {
        return GrantVerdict::BadSignature;
    }
    // The signature is authentic — body fields are now trustworthy.
    if ctx.presenting_pubkey != bound_pubkey {
        return GrantVerdict::KeyMismatch;
    }
    if body_mesh != ctx.expected_mesh {
        return GrantVerdict::WrongMesh;
    }
    if let Some(exp) = expires_at_ms {
        if ctx.now_ms >= exp {
            return GrantVerdict::Expired;
        }
    }
    GrantVerdict::Valid
}

impl SignedMeshMembership {
    /// Verify this membership against `ctx`. `Valid` ⇒ the caller may
    /// grant `attestation.default_tier` to the subject (the
    /// `mesh_identity → TrustTier` bridge, no manual step). Enforces that
    /// the presenter IS the attested subject and the mesh matches.
    pub fn verify(&self, ctx: &VerifyContext<'_>) -> GrantVerdict {
        verify_signed(
            &self.attestation,
            &self.proof,
            &self.attestation.subject_pubkey,
            &self.attestation.mesh_identity,
            self.attestation.expires_at_ms,
            ctx,
        )
    }
}

impl SignedCapabilityGrant {
    /// Verify this grant against `ctx` (issuer → signature → key → mesh →
    /// expiry). On `Valid`, [`Self::grants`] tells what it confers.
    pub fn verify(&self, ctx: &VerifyContext<'_>) -> GrantVerdict {
        verify_signed(
            &self.grant,
            &self.proof,
            &self.grant.grantee_pubkey,
            &self.grant.granted_in,
            self.grant.expires_at_ms,
            ctx,
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
    /// key, mesh, expiry, capability) is tested without a real signature.
    /// The real ed25519 verifier is a later slice; this proves the seam +
    /// policy.
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

    const OWNER: &[u8] = &[1, 1, 1];
    const GRANTEE_KEY: &[u8] = &[2, 2, 2]; // matches `grant()`'s grantee_pubkey
    const MESH: &str = "joelteply"; // matches `grant()`'s granted_in

    fn grant(caps: &[&str], expires_at_ms: Option<u64>, issuer: Vec<u8>) -> SignedCapabilityGrant {
        SignedCapabilityGrant {
            grant: CapabilityGrant {
                grantee: peer(2),
                grantee_pubkey: GRANTEE_KEY.to_vec(),
                capabilities: caps.iter().map(|c| c.to_string()).collect(),
                granted_in: MeshIdentity::new(MESH),
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

    /// Build a verify context. The referents (mesh, stub) must outlive
    /// the returned borrow — callers hold them as locals.
    fn ctx<'a>(
        now_ms: u64,
        presenting: &'a [u8],
        mesh: &'a MeshIdentity,
        stub: &'a StubVerifier,
    ) -> VerifyContext<'a> {
        VerifyContext {
            now_ms,
            presenting_pubkey: presenting,
            expected_mesh: mesh,
            verifier: stub,
            trusted_issuer_pubkey: OWNER,
        }
    }

    // what this catches: a valid grant from the trusted owner — presented
    // by the bound key, in the right mesh, unexpired, good signature —
    // verifies AND confers exactly its capability tags.
    #[test]
    fn valid_grant_verifies_and_confers_capability() {
        let (mesh, stub) = (MeshIdentity::new(MESH), StubVerifier { valid: true });
        let g = grant(&["ai/generate"], None, OWNER.to_vec());
        assert_eq!(
            g.verify(&ctx(2_000, GRANTEE_KEY, &mesh, &stub)),
            GrantVerdict::Valid
        );
        assert!(g.grants("ai/generate"));
        assert!(!g.grants("data/delete"), "grant confers only its own tags");
    }

    // what this catches (review finding 6): issuer-pinning precedes the
    // signature check — a grant signed by a NON-owner key, even with a
    // valid-per-stub signature, reports UntrustedIssuer (not BadSignature)
    // for audit fidelity. A refactor reordering the gates breaks this.
    #[test]
    fn issuer_pin_precedes_signature() {
        let (mesh, stub) = (MeshIdentity::new(MESH), StubVerifier { valid: true });
        let g = grant(&["ai/generate"], None, vec![7, 7, 7]); // not OWNER
        assert_eq!(
            g.verify(&ctx(2_000, GRANTEE_KEY, &mesh, &stub)),
            GrantVerdict::UntrustedIssuer
        );
    }

    // what this catches (review finding 3): a STOLEN grant cannot ride
    // another peer's identity — verify() enforces that the PRESENTING key
    // is the grant's bound grantee key. Without this, the key-binding is a
    // consumer-trap (only safe if every consumer remembers to cross-check).
    #[test]
    fn key_mismatch_rejects_a_stolen_grant() {
        let (mesh, stub) = (MeshIdentity::new(MESH), StubVerifier { valid: true });
        let g = grant(&["ai/generate"], None, OWNER.to_vec());
        // Presented by a key that is NOT the bound grantee.
        assert_eq!(
            g.verify(&ctx(2_000, &[9, 9, 9], &mesh, &stub)),
            GrantVerdict::KeyMismatch
        );
    }

    // what this catches (review finding 2): a grant scoped to one grid is
    // rejected when presented in another — verify() enforces granted_in ==
    // the verifier's mesh. Otherwise mesh-scoping is a consumer-trap.
    #[test]
    fn wrong_mesh_rejects_a_grant_for_another_grid() {
        let (mesh, stub) = (MeshIdentity::new("toby"), StubVerifier { valid: true });
        let g = grant(&["ai/generate"], None, OWNER.to_vec()); // granted_in joelteply
        assert_eq!(
            g.verify(&ctx(2_000, GRANTEE_KEY, &mesh, &stub)),
            GrantVerdict::WrongMesh
        );
    }

    // what this catches: an expired grant is rejected even when everything
    // else passes (now_ms >= expires_at_ms; before expiry is Valid).
    #[test]
    fn expired_grant_is_rejected() {
        let (mesh, stub) = (MeshIdentity::new(MESH), StubVerifier { valid: true });
        let g = grant(&["ai/generate"], Some(1_500), OWNER.to_vec());
        assert_eq!(
            g.verify(&ctx(2_000, GRANTEE_KEY, &mesh, &stub)),
            GrantVerdict::Expired
        );
        assert_eq!(
            g.verify(&ctx(1_500, GRANTEE_KEY, &mesh, &stub)),
            GrantVerdict::Expired
        );
        assert_eq!(
            g.verify(&ctx(1_499, GRANTEE_KEY, &mesh, &stub)),
            GrantVerdict::Valid
        );
    }

    // what this catches: a bad signature is rejected (the credential seam's
    // verdict propagates) — even from the trusted issuer, unexpired, right
    // key + mesh.
    #[test]
    fn bad_signature_is_rejected() {
        let (mesh, stub) = (MeshIdentity::new(MESH), StubVerifier { valid: false });
        let g = grant(&["ai/generate"], None, OWNER.to_vec());
        assert_eq!(
            g.verify(&ctx(2_000, GRANTEE_KEY, &mesh, &stub)),
            GrantVerdict::BadSignature
        );
    }

    // what this catches: membership attestations reuse the same verdict
    // path (issuer/sig/key/mesh/expiry), enforcing the presenter IS the
    // attested subject — so default_tier is only honored on Valid.
    #[test]
    fn mesh_membership_uses_the_same_verdict_path() {
        const SUBJECT_KEY: &[u8] = &[3, 3, 3];
        let m = SignedMeshMembership {
            attestation: MeshMembershipAttestation {
                subject: peer(3),
                subject_pubkey: SUBJECT_KEY.to_vec(),
                mesh_identity: MeshIdentity::new(MESH),
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
        let (mesh, stub) = (MeshIdentity::new(MESH), StubVerifier { valid: true });
        assert_eq!(
            m.verify(&ctx(2_000, SUBJECT_KEY, &mesh, &stub)),
            GrantVerdict::Valid
        );
        // wrong issuer -> UntrustedIssuer
        let bad = VerifyContext {
            trusted_issuer_pubkey: &[7, 7, 7],
            ..ctx(2_000, SUBJECT_KEY, &mesh, &stub)
        };
        assert_eq!(m.verify(&bad), GrantVerdict::UntrustedIssuer);
        // wrong presenter -> KeyMismatch (the anti-impersonation binding)
        assert_eq!(
            m.verify(&ctx(2_000, &[9, 9, 9], &mesh, &stub)),
            GrantVerdict::KeyMismatch
        );
        assert_eq!(m.attestation.default_tier, TrustTier::OwnAccount);
    }
}
