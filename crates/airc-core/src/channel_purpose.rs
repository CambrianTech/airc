//! Channel purpose — the substrate-published OPEN room-nature value, so
//! a citizen can be grounded in what a room is FOR (and continuum can
//! select the room's cognition recipe) without the substrate enumerating
//! a closed list of room kinds.
//!
//! ## Why this is an OPEN value, not a typed enum
//!
//! An earlier cut (#1233) made `ChannelPurpose` a closed enum
//! (`Chat | Coordination | Game | …`) intended to drive a persona's
//! should-respond gate. That was the wrong shape — a Rust enum driving
//! cognition is the `no-rust-gates-around-cognition` anti-pattern, and
//! room nature is INFINITE by construction: continuum rooms run a
//! `RecipeEntity` (recipes are DATA, not an enum of kinds), and the
//! room's purpose is just the recipe / activity key. So purpose is an
//! **open string** — the activity/recipe key the room runs.
//!
//! Layer split:
//!   - **airc** publishes + projects the open purpose value (this
//!     module). It has NO opinion on what a purpose MEANS or how a
//!     persona should behave for it.
//!   - **continuum** grounds the persona in the purpose and lets the
//!     RecipeEntity pipeline (its `ai/should-respond` step) own
//!     participation behavior — infinite, per-recipe, in the cognition
//!     layer where it belongs.
//!
//! Complementary to [`crate::doctrine`]: purpose = the open activity key
//! (what the room runs); doctrine = the free-form rules (markdown).
//!
//! Wire shape mirrors [`crate::doctrine::DoctrineEvent`]: an internally-
//! tagged event enum so a JSON body carries a `kind` field; the
//! projection (`Airc::channel_purpose`) takes the latest per room (LWW).

use crate::ids::{PeerId, RoomId};
use serde::{Deserialize, Serialize};

/// A room's OPEN purpose value — the activity / recipe key the room
/// runs (e.g. `"coordination"`, `"game:chess"`, a recipe id). Deliberately
/// NOT an enum: room nature is infinite, and continuum's RecipeEntity
/// pipeline — not a substrate type — owns what a purpose means for
/// cognition. Serializes transparently as the bare string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelPurpose(pub String);

impl ChannelPurpose {
    /// Build a purpose from any activity/recipe key.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The activity/recipe key as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ChannelPurpose {
    fn from(key: String) -> Self {
        Self(key)
    }
}

impl From<&str> for ChannelPurpose {
    fn from(key: &str) -> Self {
        Self(key.to_string())
    }
}

impl std::fmt::Display for ChannelPurpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Typed channel-purpose domain events. Internally tagged via serde so a
/// JSON body always carries a `kind` field consumers switch on — same
/// shape as [`crate::doctrine::DoctrineEvent`]. The EVENT is typed; the
/// `purpose` payload it carries is an open value. Single variant today;
/// the enum keeps the upgrade path honest (an unknown future `kind`
/// surfaces as a decode error, never a silent mis-decode).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelPurposeEvent {
    ChannelPurposePublished(ChannelPurposePublished),
}

/// A room's open purpose published on the substrate. Authority is flat
/// (per AGENTS.md §6: no role-based dispatch) — every peer may publish;
/// the projection takes the latest by `published_at_ms` (LWW).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPurposePublished {
    /// The room this purpose applies to.
    pub room_id: RoomId,
    /// The open activity/recipe key the room runs.
    pub purpose: ChannelPurpose,
    /// Peer that emitted this version. Not gating — see module doc.
    pub published_by: PeerId,
    /// Monotonic emission time. Projection takes the highest
    /// `published_at_ms` per `room_id` (LWW; ties broken by the durable
    /// log's event order).
    pub published_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn sample(purpose: &str) -> ChannelPurposePublished {
        ChannelPurposePublished {
            room_id: RoomId::from_u128(7),
            purpose: ChannelPurpose::new(purpose),
            published_by: PeerId::from_u128(42),
            published_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn channel_purpose_event_round_trips_for_arbitrary_open_values() {
        // The whole point: ANY activity/recipe key round-trips — there is
        // no closed set. A future recipe key the substrate has never seen
        // is just a string, not a decode error.
        for key in [
            "chat",
            "coordination",
            "game:chess",
            "academy:rust-101",
            "continuum:recipe:7f3a",
            "",
        ] {
            let event = ChannelPurposeEvent::ChannelPurposePublished(sample(key));
            let json = serde_json::to_string(&event).expect("serialize");
            let decoded: ChannelPurposeEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, decoded);
        }
    }

    #[test]
    fn purpose_serializes_as_a_bare_string_not_an_enum_tag() {
        // `#[serde(transparent)]` means the wire value is the open key
        // itself — `"coordination"`, never `{"coordination":...}` or a
        // closed-variant tag. Consumers read an open string.
        let value = serde_json::to_value(ChannelPurpose::new("coordination")).expect("to_value");
        assert_eq!(
            value.as_str(),
            Some("coordination"),
            "purpose is an OPEN string value, not a typed enum",
        );
    }

    #[test]
    fn event_carries_open_purpose_and_stable_kind_discriminator() {
        let event = ChannelPurposeEvent::ChannelPurposePublished(sample("game:chess"));
        let value: Value = serde_json::to_value(&event).expect("to_value");
        assert_eq!(
            value.get("kind").and_then(Value::as_str),
            Some("channel_purpose_published"),
            "event wire kind must be stable",
        );
        assert_eq!(
            value.get("purpose").and_then(Value::as_str),
            Some("game:chess"),
            "the open purpose key rides as a bare string on the event",
        );
    }

    #[test]
    fn unknown_event_kind_surfaces_as_decode_error() {
        // A future-version EVENT kind a current consumer doesn't know
        // about must error (the event enum stays honest). Distinct from
        // the PURPOSE value, which is open and never errors on an unknown
        // key.
        let raw = r#"{"kind":"channel_purpose_retired","room_id":"00000000-0000-0000-0000-000000000001"}"#;
        let result: Result<ChannelPurposeEvent, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown event kind must error");
    }
}
