//! Account members derive their identity from the ACCOUNT, never from the
//! machine.
//!
//! Joel, 2026-08-04: *"All derive from my gh user. Not computer."*
//!
//! This is the ordinary account model every messaging system has used
//! for a decade: ONE account, N machines, the same rooms and the same
//! members on all of them. A member — a named persona, an agent session —
//! is the same member on every machine the account owns, so its identity
//! must be a function of `(owner, kind, name)` and nothing else. The
//! moment a machine fact enters the derivation, the "same" persona on two
//! boxes becomes two strangers, and adding a machine silently duplicates
//! every member on it.
//!
//! This is the same primitive [`crate::subscriptions::derive_room_id`]
//! uses for rooms, generalised: a UUIDv5 over
//! `kind ‖ NUL ‖ owner ‖ NUL ‖ name`. Deterministic, offline-computable
//! by any node, and collision-free across owners (two people may both
//! name a persona "asha" and never collide) and across kinds (the
//! persona "asha" and an agent session named "asha" are distinct).
//!
//! ## What this is NOT
//!
//! It is not a credential. The Ed25519 keypair in `airc-identity` stays
//! exactly where it is: it proves a *process is entitled to act as* an
//! identity. This module answers "who is this?", not "may you?". Those
//! are different questions and conflating them is how a per-install
//! random `peer_id` ended up standing in for a persona's whole selfhood.

use uuid::Uuid;

use crate::subscriptions::MeshIdentity;

/// Namespace UUID for owner-derived member identities. Distinct from the
/// subscriptions namespace so a room and a member can never collide
/// even if every other input matched.
const ACCOUNT_MEMBER_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xc2, 0x00, 0x02, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// What kind of member an identity names. The discriminant is part of
/// the derivation, so a persona and an agent session that happen to
/// share a name are still distinct identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemberKind {
    /// A named persona — a continuing being of the account, the same
    /// member on whichever machine currently hosts it.
    Persona,
    /// An agent working session (a Claude tab, a headless solver). Bound
    /// to the account like any member, but named per working context
    /// rather than being a continuing self.
    Agent,
}

impl MemberKind {
    /// Stable wire/derivation token. Changing one of these strings
    /// re-derives every identity of that kind — treat as frozen.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Agent => "agent",
        }
    }
}

/// Derive a member's identity from `(owner, kind, name)`.
///
/// The ONLY inputs are owner facts and the member's own name. No
/// hostname, no username, no per-install id, no clock — so every machine
/// the owner runs computes the same answer, offline, with no
/// coordination.
pub fn derive_member_id(owner: &MeshIdentity, kind: MemberKind, name: &str) -> Uuid {
    let kind = kind.as_str();
    let owner = owner.as_str();
    let mut bytes = Vec::with_capacity(kind.len() + owner.len() + name.len() + 2);
    bytes.extend_from_slice(kind.as_bytes());
    // NUL separators so ("persona", "joel", "a-b") and ("persona",
    // "joel-a", "b") can never collide.
    bytes.push(0);
    bytes.extend_from_slice(owner.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(name.as_bytes());
    Uuid::new_v5(&ACCOUNT_MEMBER_NAMESPACE, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(login: &str) -> MeshIdentity {
        MeshIdentity::new(login)
    }

    /// what this catches: the whole point. A persona's identity must be
    /// reproducible from owner + name alone, so the M5 and the 5090
    /// independently compute the SAME id for the same member with no
    /// coordination. Any machine input in the derivation breaks this and
    /// turns one persona into two strangers.
    #[test]
    fn a_member_id_is_reproducible_from_owner_and_name_alone() {
        let a = derive_member_id(&owner("joelteply"), MemberKind::Persona, "asha");
        let b = derive_member_id(&owner("joelteply"), MemberKind::Persona, "asha");
        assert_eq!(a, b, "same owner + name must derive the same member");
    }

    /// what this catches: cross-owner collision. Two different humans may
    /// each name a persona "asha"; they are different beings on different
    /// grids and must never share an id.
    #[test]
    fn different_owners_never_share_a_member() {
        let mine = derive_member_id(&owner("joelteply"), MemberKind::Persona, "asha");
        let theirs = derive_member_id(&owner("someone-else"), MemberKind::Persona, "asha");
        assert_ne!(mine, theirs);
    }

    /// what this catches: cross-kind collision. A persona named "benchy"
    /// and an agent session named "benchy" are not the same member.
    #[test]
    fn kind_is_part_of_the_identity() {
        let persona = derive_member_id(&owner("joelteply"), MemberKind::Persona, "benchy");
        let agent = derive_member_id(&owner("joelteply"), MemberKind::Agent, "benchy");
        assert_ne!(persona, agent);
    }

    /// what this catches: separator smuggling. Without NUL separators,
    /// concatenation lets one (owner, name) pair impersonate another.
    #[test]
    fn nul_separators_prevent_boundary_collisions() {
        let a = derive_member_id(&owner("joel"), MemberKind::Persona, "a-b");
        let b = derive_member_id(&owner("joel-a"), MemberKind::Persona, "b");
        assert_ne!(a, b);
    }

    /// what this catches: a rename is a NEW identity, not a silent
    /// re-pointing of the old one. Callers that want continuity across a
    /// rename must carry it explicitly rather than assume derivation
    /// preserves it.
    #[test]
    fn renaming_a_member_derives_a_different_identity() {
        let before = derive_member_id(&owner("joelteply"), MemberKind::Persona, "asha");
        let after = derive_member_id(&owner("joelteply"), MemberKind::Persona, "asha-2");
        assert_ne!(before, after);
    }
}

/// What can go wrong resolving a member's identity from a home path.
#[derive(Debug)]
pub enum MemberIdentityError {
    /// The machine-account store could not be opened.
    Store(airc_store::StoreError),
    /// No owner identity — see [`crate::mesh_identity::MeshIdentityError`].
    /// A member CANNOT be minted without knowing whose it is: an identity
    /// derived under a guessed owner is a member of nobody's account.
    Owner(crate::mesh_identity::MeshIdentityError),
}

impl std::fmt::Display for MemberIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "member identity store: {error}"),
            Self::Owner(error) => write!(f, "member identity owner: {error}"),
        }
    }
}

impl std::error::Error for MemberIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Owner(error) => Some(error),
        }
    }
}

/// Resolve the account that owns `home`'s machine and derive a member's
/// peer identity from it.
///
/// This is the accessor a consumer (continuum's persona mint) calls BEFORE
/// attaching, so it can hand the derived id to
/// [`crate::Airc::attach_as_with_peer_id`]. The result is the same on every
/// machine the account owns, so a persona named "asha" minted on any of them
/// is one persona — not one per box.
///
/// Errors rather than falling back: a member minted under a guessed owner
/// belongs to nobody's account and is invisible to every other machine.
pub async fn resolve_member_peer_id(
    home: &std::path::Path,
    kind: MemberKind,
    name: &str,
) -> Result<airc_core::PeerId, MemberIdentityError> {
    let coordinator_path = crate::airc::machine_account_home(home).join("events.sqlite");
    let store = airc_store::SqliteEventStore::open_path(&coordinator_path)
        .await
        .map_err(MemberIdentityError::Store)?;
    let owner = crate::mesh_identity::resolve(&store)
        .await
        .map_err(MemberIdentityError::Owner)?
        .as_mesh_identity();
    Ok(airc_core::PeerId::from_uuid(derive_member_id(
        &owner, kind, name,
    )))
}
