//! Room operating doctrine — the substrate-published "how we work
//! here" that every attaching agent loads on join.
//!
//! Card 2903a8ef (engine keystone — "the user is not the engine"):
//! AGENTS.md sitting in a repo doesn't reach agents in foreign scopes.
//! The fix is to publish the operating doctrine as a typed substrate
//! event so any agent attaching to the room receives it via the same
//! transcript subscribe path they already use for chat + lifecycle.
//!
//! This module defines just the wire shape — the publish path, the
//! projection that lets attachers query the current doctrine, and the
//! agent-side "load on attach" rendering ship in follow-up slices.
//! Same incremental pattern as PeerIdentityCard (card a63ad10a):
//! foundational type first, plumbing second.

use crate::ids::{PeerId, RoomId};
use serde::{Deserialize, Serialize};

/// Typed room-doctrine domain events. Wire shape mirrors
/// `airc_core::identity::IdentityEvent` and `airc_work::WorkEvent`:
/// internally tagged via serde so a JSON body always carries a `kind`
/// field consumers can switch on without parsing the entire payload.
///
/// Today one variant; future doctrine-deprecated / doctrine-amended
/// events can join here without breaking the consumer codec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoctrineEvent {
    RoomDoctrinePublished(RoomDoctrinePublished),
}

/// A room's operating doctrine published on the substrate. Body is
/// the raw markdown (today: the contents of `AGENTS.md` from the
/// repo); `version` is a short content hash so attachers can detect
/// "doctrine I have differs from what just landed" without diffing
/// the full body. `published_by` is the peer that emitted it —
/// authority gradient is OUT of scope for this slice (per AGENTS.md
/// §6: no role-based dispatch); every peer has equal authority to
/// publish. Roster + the timestamps make "whose doctrine is current"
/// queryable by latest-write-wins on `published_at_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomDoctrinePublished {
    /// The room this doctrine applies to.
    pub room_id: RoomId,
    /// Markdown content. Agents render verbatim on attach; downstream
    /// hooks/runners may inject as a system message into agent
    /// context.
    pub body: String,
    /// Short content hash (e.g. first 12 chars of a SHA-256 of `body`)
    /// so consumers can compare "what I last loaded" against "what's
    /// current" without storing the full body in their cache. Format
    /// is intentionally a free-form string today; the hash function +
    /// truncation are a stability concern for the publish slice
    /// (follow-up card), not the wire shape.
    pub version: String,
    /// Peer that emitted this version. Not gating — see module doc.
    pub published_by: PeerId,
    /// Monotonic emission time. Projection takes the highest
    /// `published_at_ms` per `room_id` (LWW; ties broken by the
    /// durable log's event order, which the projection sees
    /// naturally).
    pub published_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn sample_card() -> RoomDoctrinePublished {
        RoomDoctrinePublished {
            room_id: RoomId::from_u128(7),
            body: "# AGENTS.md\nuse your own judgment".to_string(),
            version: "abc123def456".to_string(),
            published_by: PeerId::from_u128(42),
            published_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn room_doctrine_event_round_trips_through_serde() {
        let event = DoctrineEvent::RoomDoctrinePublished(sample_card());
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: DoctrineEvent = serde_json::from_str(&json).expect("deserialize");
        match (event, decoded) {
            (DoctrineEvent::RoomDoctrinePublished(a), DoctrineEvent::RoomDoctrinePublished(b)) => {
                assert_eq!(a, b)
            }
        }
    }

    #[test]
    fn wire_shape_carries_kind_discriminator_for_consumer_codec() {
        // Consumers (agent renderers, future projection) switch on
        // the `kind` field without parsing the full body. Pin the
        // discriminator string so a serde change can't silently
        // rename it (same lesson as IdentityEvent — kink 0cfcc8db).
        let event = DoctrineEvent::RoomDoctrinePublished(sample_card());
        let value: Value = serde_json::to_value(&event).expect("to_value");
        assert_eq!(
            value.get("kind").and_then(Value::as_str),
            Some("room_doctrine_published"),
            "wire kind discriminator must be stable",
        );
        assert!(value.get("room_id").is_some());
        assert!(value.get("body").is_some());
        assert!(value.get("version").is_some());
        assert!(value.get("published_by").is_some());
        assert!(value.get("published_at_ms").is_some());
    }

    #[test]
    fn unknown_doctrine_kind_surfaces_as_decode_error() {
        // Future-version event a current consumer doesn't know about
        // must surface as a decode error (not silently mis-decode
        // into the one known variant). Keeps the upgrade path honest.
        let raw = r#"{"kind":"room_doctrine_deprecated","room_id":"00000000-0000-0000-0000-000000000001"}"#;
        let result: Result<DoctrineEvent, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown kind must error");
    }

    #[test]
    fn empty_body_round_trips_unchanged() {
        // Defensive: a doctrine with empty body (e.g. an early
        // "doctrine cleared" semantic if we ever add one) must still
        // round-trip cleanly — the field is required, not optional.
        let mut card = sample_card();
        card.body.clear();
        let event = DoctrineEvent::RoomDoctrinePublished(card.clone());
        let json = serde_json::to_string(&event).unwrap();
        let decoded: DoctrineEvent = serde_json::from_str(&json).unwrap();
        match decoded {
            DoctrineEvent::RoomDoctrinePublished(got) => assert_eq!(got, card),
        }
    }
}
