//! Grid identities derive from the OWNER, never from the machine.
//!
//! Joel, 2026-08-04: *"All derive from my gh user. Not computer."*
//!
//! A citizen of a grid — a named persona, an agent session — is the same
//! citizen on every machine that owner runs. So its identity must be a
//! function of `(owner, kind, name)` and nothing else. The moment a
//! machine fact enters the derivation, the "same" persona on two boxes
//! becomes two beings that cannot recognise each other, and the grid is
//! a set of islands wearing matching name tags.
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

/// Namespace UUID for owner-derived grid identities. Distinct from the
/// subscriptions namespace so a room and a citizen can never collide
/// even if every other input matched.
const GRID_IDENTITY_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xc2, 0x00, 0x02, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// What kind of citizen an identity names. The discriminant is part of
/// the derivation, so a persona and an agent session that happen to
/// share a name are still distinct identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CitizenKind {
    /// A named persona — a continuing being of the owner's grid, the
    /// same citizen on whichever machine currently hosts it.
    Persona,
    /// An agent working session (a Claude tab, a headless solver). Bound
    /// to the owner like any citizen, but named per working context
    /// rather than being a continuing self.
    Agent,
}

impl CitizenKind {
    /// Stable wire/derivation token. Changing one of these strings
    /// re-derives every identity of that kind — treat as frozen.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Agent => "agent",
        }
    }
}

/// Derive a citizen's identity from `(owner, kind, name)`.
///
/// The ONLY inputs are owner facts and the citizen's own name. No
/// hostname, no username, no per-install id, no clock — so every machine
/// the owner runs computes the same answer, offline, with no
/// coordination.
pub fn derive_citizen_id(owner: &MeshIdentity, kind: CitizenKind, name: &str) -> Uuid {
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
    Uuid::new_v5(&GRID_IDENTITY_NAMESPACE, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(login: &str) -> MeshIdentity {
        MeshIdentity::new(login)
    }

    /// what this catches: the whole point. A persona's identity must be
    /// reproducible from owner + name alone, so the M5 and the 5090
    /// independently compute the SAME id for the same citizen with no
    /// coordination. Any machine input in the derivation breaks this and
    /// turns one persona into two strangers.
    #[test]
    fn a_citizen_id_is_reproducible_from_owner_and_name_alone() {
        let a = derive_citizen_id(&owner("joelteply"), CitizenKind::Persona, "asha");
        let b = derive_citizen_id(&owner("joelteply"), CitizenKind::Persona, "asha");
        assert_eq!(a, b, "same owner + name must derive the same citizen");
    }

    /// what this catches: cross-owner collision. Two different humans may
    /// each name a persona "asha"; they are different beings on different
    /// grids and must never share an id.
    #[test]
    fn different_owners_never_share_a_citizen() {
        let mine = derive_citizen_id(&owner("joelteply"), CitizenKind::Persona, "asha");
        let theirs = derive_citizen_id(&owner("someone-else"), CitizenKind::Persona, "asha");
        assert_ne!(mine, theirs);
    }

    /// what this catches: cross-kind collision. A persona named "benchy"
    /// and an agent session named "benchy" are not the same citizen.
    #[test]
    fn kind_is_part_of_the_identity() {
        let persona = derive_citizen_id(&owner("joelteply"), CitizenKind::Persona, "benchy");
        let agent = derive_citizen_id(&owner("joelteply"), CitizenKind::Agent, "benchy");
        assert_ne!(persona, agent);
    }

    /// what this catches: separator smuggling. Without NUL separators,
    /// concatenation lets one (owner, name) pair impersonate another.
    #[test]
    fn nul_separators_prevent_boundary_collisions() {
        let a = derive_citizen_id(&owner("joel"), CitizenKind::Persona, "a-b");
        let b = derive_citizen_id(&owner("joel-a"), CitizenKind::Persona, "b");
        assert_ne!(a, b);
    }

    /// what this catches: a rename is a NEW identity, not a silent
    /// re-pointing of the old one. Callers that want continuity across a
    /// rename must carry it explicitly rather than assume derivation
    /// preserves it.
    #[test]
    fn renaming_a_citizen_derives_a_different_identity() {
        let before = derive_citizen_id(&owner("joelteply"), CitizenKind::Persona, "asha");
        let after = derive_citizen_id(&owner("joelteply"), CitizenKind::Persona, "asha-2");
        assert_ne!(before, after);
    }
}
