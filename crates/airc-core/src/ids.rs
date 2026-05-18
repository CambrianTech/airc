//! Newtype-wrapped identifier types.
//!
//! Every internal id in airc-rust is a UUIDv4 — the right primitive for
//! a P2P mesh substrate where peers generate ids locally without a
//! central coordinator. See the substrate design doc, section
//! "Identifiers — UUIDv4 everywhere," for the full rationale.
//!
//! Wire shape: serde encodes `Uuid` as the canonical hyphenated string
//! (`"550e8400-e29b-41d4-a716-446655440000"`), so JSON envelopes stay
//! human-readable and cross-language. The `#[serde(transparent)]`
//! attribute means the JSON shape is just the bare string — no
//! `{"value": ...}` wrapper.
//!
//! Generation: `EventId::new()`, `RoomId::new()`, etc. all delegate to
//! `Uuid::new_v4()`. Tests that need deterministic ids use
//! `EventId::from_u128(N)` (also exposed below).
//!
//! Exception: `ContentHash` is NOT a UUID — it carries a content-
//! addressed hash like `"sha256:<hex>"`. Content addressing is the
//! discipline for blobs; UUIDs are the discipline for runtime
//! identifiers. They don't overlap.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh random UUIDv4.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Construct from a known u128 — for deterministic test ids.
            /// Production code should use `new()`.
            pub fn from_u128(value: u128) -> Self {
                Self(Uuid::from_u128(value))
            }

            /// Construct from an existing Uuid (e.g. one parsed from
            /// the wire).
            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Access the underlying Uuid.
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_newtype! {
    /// Stable identifier for one transcript event. Persists across host
    /// migrations and replay — two peers receiving the same wire envelope
    /// see the same `EventId`.
    EventId
}

uuid_newtype! {
    /// A room / channel handle. Peers reference rooms internally by
    /// `RoomId`; display names (`#general`) are mutable handles on top.
    RoomId
}

uuid_newtype! {
    /// A peer identifier — the canonical "who is this." Multiple
    /// `ClientId`s may share one `PeerId` (multi-tab same-identity case).
    PeerId
}

uuid_newtype! {
    /// A per-process / per-tab session identifier under a peer. The pair
    /// `(PeerId, ClientId)` uniquely identifies one running airc consumer
    /// session. Used for self-echo filtering when multiple sessions share
    /// a nick.
    ClientId
}

uuid_newtype! {
    /// Stable handle for an attached file or media manifest. The blob
    /// content itself is addressed by `ContentHash` (sha256); this id
    /// is the manifest handle.
    FileId
}

/// Content-addressed hash of a blob. Format-neutral string (typically
/// `"sha256:<hex>"` but consumers may use other prefixes for other
/// algorithms — airc just matches strings).
///
/// NOT a UUID. Content addressing has different requirements than
/// runtime identifiers: collision = identical content (a feature, not
/// a bug), generation = hash of payload bytes (not random). UUIDs
/// would defeat both properties.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_event_id_is_random_uuidv4() {
        let a = EventId::new();
        let b = EventId::new();
        assert_ne!(a, b, "two new EventIds must differ (collision odds 2^-122)");
        assert_eq!(
            a.0.get_version(),
            Some(uuid::Version::Random),
            "EventId must be UUIDv4"
        );
    }

    #[test]
    fn from_u128_is_deterministic_for_tests() {
        let a = EventId::from_u128(0xdead_beef_dead_beef_dead_beef_dead_beef);
        let b = EventId::from_u128(0xdead_beef_dead_beef_dead_beef_dead_beef);
        assert_eq!(a, b, "from_u128 with same value must be equal");
    }

    #[test]
    fn serde_roundtrips_via_hyphenated_string() {
        let id = EventId::from_u128(0x550e8400_e29b_41d4_a716_446655440000);
        let encoded = serde_json::to_value(id).unwrap();
        // Tagged as a bare string in JSON (serde transparent + Uuid's
        // own serializer produces hyphenated lowercase).
        assert_eq!(encoded, "550e8400-e29b-41d4-a716-446655440000");
        let decoded: EventId = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, id);
    }

    #[test]
    fn display_uses_hyphenated_lowercase() {
        let id = PeerId::from_u128(0x550e8400_e29b_41d4_a716_446655440000);
        assert_eq!(format!("{}", id), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn malformed_uuid_string_fails_parse() {
        // Forward-compat safety: if a legacy or hostile wire envelope
        // sends `"event_id": "not-a-uuid"`, we want a typed parse error,
        // not a silent String coercion deep in the routing path.
        let result: Result<EventId, _> = serde_json::from_value(serde_json::json!("not-a-uuid"));
        assert!(result.is_err(), "malformed UUID must fail to parse");
    }

    #[test]
    fn all_id_newtypes_share_the_uuidv4_contract() {
        // Macro produces identical behavior across all newtype IDs.
        // This test pins that contract.
        assert_eq!(EventId::new().0.get_version(), Some(uuid::Version::Random));
        assert_eq!(RoomId::new().0.get_version(), Some(uuid::Version::Random));
        assert_eq!(PeerId::new().0.get_version(), Some(uuid::Version::Random));
        assert_eq!(ClientId::new().0.get_version(), Some(uuid::Version::Random));
        assert_eq!(FileId::new().0.get_version(), Some(uuid::Version::Random));
    }

    #[test]
    fn content_hash_is_not_uuid_typed() {
        // ContentHash is intentionally a String — content-addressed
        // hashes have different requirements (deterministic + payload-
        // derived) than runtime ids. This test pins that distinction.
        let h = ContentHash("sha256:abc123".to_string());
        let encoded = serde_json::to_value(&h).unwrap();
        assert_eq!(encoded, "sha256:abc123");
    }
}
