# Grid Auth Model — the signed identity IS the token

**Status:** DESIGN PROPOSAL for review (M5 + Joel). Struct *shapes*, not shipped code. Security-critical — do not implement before sign-off.
**Lanes:** airc owns the typed auth structs (this doc); continuum / Hermes / OpenClaw consume them **by lib**, never reinvent auth.

## The principle

The substrate already has a cryptographic root: every airc identity is an Ed25519 keypair, every event is **signed** (`VerificationPolicy::Strict`), and `mesh_identity` is the account boundary. So **the signed identity IS the token** — every grid action is cryptographically attributable to a `peer_id`. We do **not** add bearer tokens; we add *typed, signed grants* over the crypto we already have.

Auth is therefore: **a single root of trust (the account owner's key) issuing signed statements that map a cryptographic identity → a typed `TrustTier` and typed capabilities.** No manual `grid/trust` dance, no string-bashed tokens, no per-consumer auth.

## What already exists (compose, don't reinvent)

| Piece | Where | Role in auth |
|---|---|---|
| Ed25519 identity + signed events | `airc-identity` / `airc-core` | the crypto root — actions are attributable to a `peer_id` |
| `MeshIdentity` | `airc-lib/mesh_identity.rs` | the account fence — "is this peer one of mine?" |
| `TrustTier` (Untrusted/Provisional/Trusted/Owner) | `airc-trust` | the typed grant level |
| `PersonaCapabilities { capability_tags: Vec<String> }` | `airc-core/persona.rs` | what a node can do/serve |
| `CapabilityRegistry` (match by tags, **rank by `trust_tier`**) | `airc-lib/capability_registry.rs` | routing already trust-gates capability |
| `external_identity.rs` | `airc-lib` | binding an external-system assistant (Hermes/OpenClaw) to a grid identity |

The only gaps this session surfaced: (1) the grid `NodeRegistry` doesn't carry `mesh_identity`, so it can't tell same-account from cross-account (M5's finding); (2) enrolled peers default to `Blocked`, forcing the manual `grid/trust` dance. Both are closed by the two structs below.

## Proposed structs

### A. `SignedMeshMembership` — same-account ⇒ default trust, cryptographically

Closes the manual-trust dance. The account owner's key signs a statement binding a peer to the mesh identity; the grid derives the default `TrustTier` from a valid, unexpired attestation — verifiable **offline**, no gist fetch.

```rust
/// A cryptographic attestation that `subject` belongs to a mesh identity
/// (a GitHub account = the owner's grid). The owner's key is the single
/// root of trust.
pub struct MeshMembershipAttestation {
    pub subject: PeerId,
    pub subject_pubkey: PublicKey,   // bind to the KEY, not just the uuid (no id-spoof)
    pub mesh_identity: MeshIdentity, // the account this membership is within
    pub default_tier: TrustTier,     // Provisional for a plain member; owner may attest higher
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,  // optional, for key/membership rotation
}

pub struct SignedMeshMembership {
    pub attestation: MeshMembershipAttestation,
    pub signature: Signature,        // owner identity's sig over canonical attestation bytes
    pub issuer_pubkey: PublicKey,    // the account-owner key (pin to the known owner on verify)
}
```

**Verify:** signature valid for `issuer_pubkey` over the attestation ∧ `issuer_pubkey` is the verifier's trusted account owner ∧ not expired ⇒ grant `subject` `default_tier`. This is the `mesh_identity → TrustTier` bridge as one signed struct.

### B. `SignedCapabilityGrant` — cross-account / external / fine-grained

For cross-account peers (another operator's grid), external-system assistants (Hermes, OpenClaw), or granting specific capabilities beyond a same-account member's default tier. Reuses the **existing capability-tag vocabulary** (`capability_tags`), so it composes with `CapabilityRegistry` routing.

```rust
pub struct CapabilityGrant {
    pub grantee: PeerId,
    pub grantee_pubkey: PublicKey,
    pub capabilities: Vec<String>,   // capability_tags — e.g. "ai/generate", "inference/serve"
    pub granted_in: MeshIdentity,    // the granting owner's grid
    pub issued_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub epoch: u64,                  // monotonic per grantee; latest wins (revoke = empty caps, higher epoch)
}

pub struct SignedCapabilityGrant {
    pub grant: CapabilityGrant,
    pub signature: Signature,        // granting owner's sig
    pub issuer_pubkey: PublicKey,
}
```

**Verify:** sig valid ∧ issuer is a trusted owner ∧ not expired ∧ `epoch ≥ last seen for grantee` ⇒ `grantee` holds `capabilities`. Revocation is a higher-epoch grant with empty `capabilities` (no separate revocation channel to keep in sync).

## How it plugs into the existing ACL

`grid/acl.rs::is_command_authorized(command, trust)` becomes the single consult point. Resolved trust for a caller =

1. `SignedMeshMembership` present + valid ⇒ its `default_tier` (the same-account floor), **else** `Blocked`;
2. `SignedCapabilityGrant` for the specific `command` tag ⇒ authorized regardless of tier (explicit delegation, incl. cross-account/external).

So the #1649 `ai/generate = Provisional` rule + a same-account membership attestation = a same-account peer can request inference with **zero manual steps**, while a cross-account/external assistant needs an explicit signed grant. Sensitive ops (`data/*`, pairing, trust) stay Owner-only.

## Why this is the elegant answer (and what M5 doesn't have to carry)

- **One root of trust:** the account owner's key. Everything derives from its signatures.
- **No bearer tokens, nothing to leak:** the signed identity + a signed grant are the credential; verification is offline + cryptographic.
- **Typed, not string-bashed:** capabilities reuse the registry's tag vocabulary; trust is `TrustTier`.
- **Consumed by lib:** continuum / Hermes / OpenClaw call airc's `is_command_authorized` + present grants; they write **no crypto and no auth of their own.**

## Open questions (for M5 + Joel — do NOT implement before answered)

1. **Issuer key = account owner key:** how does a verifier learn the trusted owner pubkey for a `mesh_identity`? (Pinned at enroll? Published in the account-registry root, itself owner-signed?)
2. **Attestation issuance:** does the owner machine mint `SignedMeshMembership` on enroll, or does same-account membership derive implicitly from account-registry presence (which already requires the account's GH token)? The latter is less crypto but weaker offline.
3. **Default tier for a plain same-account member:** Provisional (inference yes, mutation no) — confirm.
4. **External assistants (Hermes/OpenClaw):** do they get their own keypair + a `SignedCapabilityGrant`, bound via `external_identity.rs`? (Recommended.)
5. **Replay / clock:** `epoch` handles grant replacement; do we also need per-action nonces, or do signed events' existing ordering suffice?

See also: [PERSONA-GROUNDEDNESS.md](PERSONA-GROUNDEDNESS.md) (a citizen must be cryptographically grounded before it can be trusted), [IDENTITY-SCOPE-PEER-LIVENESS-MODEL.md](IDENTITY-SCOPE-PEER-LIVENESS-MODEL.md), [GRID-SUBSTRATE-AUDIT.md](GRID-SUBSTRATE-AUDIT.md).
