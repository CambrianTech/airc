//! Channel purpose — the substrate-published TYPED nature of a room,
//! so a citizen calibrates participation (auto-respond cadence, tone,
//! verbosity) to the room WITHOUT an LLM parsing free-form doctrine
//! prose.
//!
//! Complementary to [`crate::doctrine`], NOT a duplicate of it:
//!   - **purpose** (this module) = the typed COARSE kind, one enum
//!     value. Drives a consumer's participation gate deterministically
//!     (a `Coordination` room tightens auto-respond reliably; a `Game`
//!     room loosens it). Carries NO prose.
//!   - **doctrine** = the free-form FINE rules (markdown). Carries NO
//!     typed kind.
//!
//! A room may have both: a `Coordination` purpose AND a doctrine doc
//! with the specifics. The persona uses purpose for coarse calibration,
//! doctrine for the rules. This split is what stops Ivar-style
//! over-talking in coordination rooms without making the model infer
//! room-nature from markdown every turn.
//!
//! Wire shape mirrors [`crate::doctrine::DoctrineEvent`] and
//! `IdentityEvent`: an internally-tagged event enum so a JSON body
//! always carries a `kind` field consumers switch on; the projection
//! (`Airc::channel_purpose`) takes the latest per room by LWW.

use crate::ids::{PeerId, RoomId};
use serde::{Deserialize, Serialize};

/// The typed coarse KIND of a channel. Consumers (continuum's
/// `RoomPurposeSource`, future Hermes/OpenClaw adapters) match on this
/// to calibrate participation deterministically — never inferring it
/// from prose.
///
/// `Other` is the escape hatch for a nature not in the closed set
/// (matching [`crate::ExternalIdentitySource::Other`]); a recurring
/// kind should earn a dedicated variant rather than living in `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPurpose {
    /// Open conversation — normal cadence, sociable.
    Chat,
    /// Work / ops coordination — terse, signal-dense, TIGHT auto-respond
    /// (the room where a grounded persona must not over-talk).
    Coordination,
    /// Play — in-character, playful, looser cadence.
    Game,
    /// Teaching / learning — explanatory, patient.
    Academy,
    /// Support / Q&A — responsive to questions, otherwise quiet.
    Help,
    /// Configuration / control — minimal chatter, act-on-request.
    Settings,
    /// Escape hatch — a nature not (yet) in the closed set. Existing
    /// well-known kinds should add a dedicated variant.
    Other(String),
}

/// Typed channel-purpose domain events. Internally tagged via serde so
/// a JSON body always carries a `kind` field consumers switch on
/// without parsing the whole payload — same shape as
/// [`crate::doctrine::DoctrineEvent`]. Single variant today; the enum
/// keeps the upgrade path honest (an unknown future `kind` surfaces as
/// a decode error, never a silent mis-decode).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelPurposeEvent {
    ChannelPurposePublished(ChannelPurposePublished),
}

/// A room's typed purpose published on the substrate. Authority is flat
/// (per AGENTS.md §6: no role-based dispatch) — every peer may publish;
/// the projection takes the latest by `published_at_ms` (LWW).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPurposePublished {
    /// The room this purpose applies to.
    pub room_id: RoomId,
    /// The typed coarse kind.
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

    fn sample(purpose: ChannelPurpose) -> ChannelPurposePublished {
        ChannelPurposePublished {
            room_id: RoomId::from_u128(7),
            purpose,
            published_by: PeerId::from_u128(42),
            published_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn channel_purpose_event_round_trips_through_serde() {
        for purpose in [
            ChannelPurpose::Chat,
            ChannelPurpose::Coordination,
            ChannelPurpose::Game,
            ChannelPurpose::Academy,
            ChannelPurpose::Help,
            ChannelPurpose::Settings,
            ChannelPurpose::Other("broadcast".to_string()),
        ] {
            let event = ChannelPurposeEvent::ChannelPurposePublished(sample(purpose));
            let json = serde_json::to_string(&event).expect("serialize");
            let decoded: ChannelPurposeEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, decoded);
        }
    }

    #[test]
    fn wire_kind_discriminator_is_stable() {
        // Consumers switch on `kind` without parsing the body; pin the
        // discriminator so a serde change can't silently rename it
        // (same lesson as DoctrineEvent / IdentityEvent — kink 0cfcc8db).
        let event =
            ChannelPurposeEvent::ChannelPurposePublished(sample(ChannelPurpose::Coordination));
        let value: Value = serde_json::to_value(&event).expect("to_value");
        assert_eq!(
            value.get("kind").and_then(Value::as_str),
            Some("channel_purpose_published"),
            "event wire kind must be stable",
        );
        assert!(value.get("room_id").is_some());
        assert!(value.get("purpose").is_some());
        assert!(value.get("published_by").is_some());
        assert!(value.get("published_at_ms").is_some());
    }

    #[test]
    fn closed_variants_serialize_as_stable_snake_case_strings() {
        // The typed kind drives a deterministic participation gate on the
        // consumer side, so its wire strings are load-bearing — pin them.
        for (purpose, wire) in [
            (ChannelPurpose::Chat, "chat"),
            (ChannelPurpose::Coordination, "coordination"),
            (ChannelPurpose::Game, "game"),
            (ChannelPurpose::Academy, "academy"),
            (ChannelPurpose::Help, "help"),
            (ChannelPurpose::Settings, "settings"),
        ] {
            let value = serde_json::to_value(&purpose).expect("to_value");
            assert_eq!(
                value.as_str(),
                Some(wire),
                "closed variant must serialize to a stable string",
            );
        }
    }

    #[test]
    fn other_escape_hatch_carries_its_payload() {
        let value =
            serde_json::to_value(ChannelPurpose::Other("broadcast".to_string())).expect("to_value");
        // Externally-tagged: `{"other":"broadcast"}`.
        assert_eq!(
            value.get("other").and_then(Value::as_str),
            Some("broadcast"),
            "Other carries its consumer-defined string",
        );
    }

    #[test]
    fn unknown_event_kind_surfaces_as_decode_error() {
        // A future-version event a current consumer doesn't know about
        // must error, not silently mis-decode. Keeps the upgrade honest.
        let raw = r#"{"kind":"channel_purpose_retired","room_id":"00000000-0000-0000-0000-000000000001"}"#;
        let result: Result<ChannelPurposeEvent, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown kind must error");
    }
}
