//! Goal-lifecycle event shapes + the typed `CardOrigin` field that
//! lands on `CardCreated` (card e4cad280, slice C1).
//!
//! Slice C1 ships only the wire-stable typed shapes:
//! - `CardOrigin` enum (Manual / Synthesized / External) — the typed
//!   provenance field that lives on `CardCreated.origin` per v2 A1.
//! - `GoalCreated` / `GoalAchieved` / `GoalAbandoned` /
//!   `GoalDryTickRecorded` — the goal-lifecycle events the projection
//!   (slice C2) will apply.
//!
//! Projection logic (first-write-wins dedup arbitration, GoalState
//! transitions, DryForTicks tick counting, `recipe_refs` scoping on
//! Goal) lands in C2. The synthesizer that EMITS these events lands
//! in C3. This module is pure wire shape + serde round-trips so the
//! event format gets nailed down before the projection bakes against
//! it and the synthesizer commits to emitting it.
//!
//! ## Doctrines
//!
//! - `[[strong-typing-across-boundaries]]` — `CardOrigin` is a typed
//!   tagged enum, not a string; the variants carry exactly the typed
//!   ids/refs the projection needs to arbitrate. No prose-tagged
//!   provenance.
//! - **No-anonymity (v2 A1, positron #1602 precedent)** — every card's
//!   origin lives on the card itself, not on a side-channel event the
//!   projection would have to join. Once the field is non-`None`,
//!   replay tells you where a card came from without a second event.
//! - **Append-only events** — same shape convention as the existing
//!   `event.rs` types (`#[derive(... Serialize, Deserialize)]`,
//!   `*_at_ms: u64` for time, `*_by: PeerId` for attribution).

use airc_core::PeerId;
use serde::{Deserialize, Serialize};

use crate::goal::{ExitCondition, GoalId};
use crate::ids::RepoId;
use crate::recipe::RecipeRef;

/// Provenance of a `CardCreated` event. Lives on the card itself so
/// every card carries the answer to "where did this come from?"
/// without joining a separate event stream (v2 A1).
///
/// Wire-shape-stable. `#[serde(tag = "kind", rename_all = "snake_case")]`
/// matches the convention used by `WorkEvent` and `GoalState` so the
/// JSON shape is consistent across the crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardOrigin {
    /// Created by a human or persona acting directly. The most common
    /// arm today — every card filed via `airc work create` lands here.
    /// The peer id is duplicate of `CardCreated.created_by`, kept here
    /// so the typed variant is self-contained (no requirement to look
    /// at sibling fields to know the human's identity).
    Manual { peer: PeerId },
    /// Created by a `Synthesizer` running a `Recipe` against a `Goal`.
    /// All four fields are projection input: `goal_id` keys the goal
    /// the proposal counted toward; `recipe_id` attributes the audit
    /// trail; `synthesizer_peer` answers "which peer was running the
    /// idle tick when this was minted"; `dedup_key` is the recipe-
    /// provided equivalence relation the projection's first-write-wins
    /// arbitration (slice C2) keys off (v2 A4a).
    Synthesized {
        goal_id: GoalId,
        recipe_id: RecipeRef,
        synthesizer_peer: PeerId,
        dedup_key: String,
    },
    /// Created by an external bridge (gh issue mirror, jira import,
    /// etc.). `source` names the bridge; `foreign_id` is the
    /// bridge-native identifier (issue number, jira key, etc.). The
    /// projection treats `External`-origin cards as immutable from
    /// airc's POV — the bridge owns the lifecycle.
    External {
        source: ExternalSource,
        foreign_id: String,
    },
}

/// The bridge that introduced an `External` card. Named explicitly
/// (typed variant rather than free string) so adding a new bridge is
/// a deliberate doc + projection change, not a silent surface
/// expansion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExternalSource {
    /// GitHub issue mirror.
    GhIssue,
    /// GitHub PR mirror (e.g. a PR opened without a paired card).
    GhPullRequest,
}

/// A new goal exists. Slice C2's projection materializes the `Goal`
/// from this event; until then it's purely wire-shape.
///
/// `recipe_refs` declares which recipes are eligible to propose for
/// this goal (slice C3's synthesizer scopes dispatch per goal). Empty
/// at creation is valid — the projection accepts it, and the recipe
/// set can grow via a follow-up `GoalUpdated` event (not in C1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCreated {
    pub goal_id: GoalId,
    pub title: String,
    pub default_repo: RepoId,
    pub exit_condition: ExitCondition,
    /// Recipes eligible to propose for this goal. Empty Vec means
    /// "no synthesis until at least one recipe is bound" — the
    /// synthesizer (C3) skips goals with empty `recipe_refs`.
    #[serde(default)]
    pub recipe_refs: Vec<RecipeRef>,
    pub created_by: PeerId,
    pub created_at_ms: u64,
}

/// A goal reached its `ExitCondition` and the projection transitioned
/// it to `GoalState::Achieved`. The projection emits this event when
/// the typed `ExitCondition` (slice A's `Goal.exit_condition` field)
/// fires; recipes do NOT emit `GoalAchieved` directly — that would
/// break the no-fallbacks-ever rule (recipes could lie about the
/// condition).
///
/// The `condition` field records WHICH condition fired (for the audit
/// trail). For `MilestoneClosed` / `AllCardsClosed` the projection
/// can attribute deterministically; for `DryForTicks` the projection
/// fires this after observing N consecutive `GoalDryTickRecorded`
/// events (v2 residual 1 fix).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAchieved {
    pub goal_id: GoalId,
    /// Which exit condition fired. Subset of the goal's declared
    /// `ExitCondition` — never `OperatorOnly` (operator-only goals
    /// transition via the manual path that doesn't emit this event).
    pub condition: ExitCondition,
    pub achieved_at_ms: u64,
}

/// An operator marked a goal abandoned. Distinguishable from
/// `GoalAchieved` in the audit trail; both transitions stop the
/// synthesizer from proposing for the goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAbandoned {
    pub goal_id: GoalId,
    pub abandoned_by: PeerId,
    pub reason: String,
    pub abandoned_at_ms: u64,
}

/// The synthesizer ran a tick for this goal and EVERY registered
/// recipe (scoped by `Goal.recipe_refs`) produced zero proposals.
///
/// Required for the `ExitCondition::DryForTicks { n }` evaluator
/// (v2 residual 1): dry ticks emit no other event the projection
/// could count, so the synthesizer emits this typed event whenever
/// a tick goes dry. The projection (C2) counts consecutive dry-tick
/// events per goal; reaching `n` fires `GoalAchieved { condition:
/// DryForTicks { n } }`. Non-`DryForTicks` goals also produce these
/// events (no harm; projection ignores), so the synthesizer doesn't
/// need a per-goal branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDryTickRecorded {
    pub goal_id: GoalId,
    pub synthesizer_peer: PeerId,
    pub recorded_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::ExitCondition;
    use crate::ids::{RepoId, WorkCardId};

    fn peer() -> PeerId {
        PeerId::new()
    }

    fn repo() -> RepoId {
        RepoId::new("CambrianTech/airc").expect("static repo id")
    }

    #[test]
    fn card_origin_manual_round_trips() {
        // what this catches: regression where the `Manual { peer }`
        // variant's wire shape drifts. The projection arbitrates dedup
        // on `Synthesized.dedup_key`, but every variant must replay
        // identically to its emission; otherwise the audit trail
        // becomes inconsistent across replayers.
        let origin = CardOrigin::Manual { peer: peer() };
        let json = serde_json::to_string(&origin).expect("serialize");
        let parsed: CardOrigin = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, origin);
    }

    #[test]
    fn card_origin_synthesized_carries_all_four_fields() {
        // what this catches: regression where any of the four
        // projection-input fields drops from `Synthesized`. The
        // projection's first-write-wins dedup (slice C2) keys on
        // `(goal_id, dedup_key)`; the audit trail keys on
        // `recipe_id + synthesizer_peer`. Dropping any breaks
        // arbitration OR provenance.
        let origin = CardOrigin::Synthesized {
            goal_id: GoalId::new(),
            recipe_id: RecipeRef::new("follow-up-extraction"),
            synthesizer_peer: peer(),
            dedup_key: "e4cad280::slice-c::dedup-arb".into(),
        };
        let json = serde_json::to_string(&origin).expect("serialize");
        let parsed: CardOrigin = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, origin);
    }

    #[test]
    fn card_origin_external_round_trips() {
        // what this catches: regression where the `External` variant's
        // wire shape drifts — bridges (gh issue mirror, etc.) need a
        // stable wire format so replays of mirrored cards are
        // deterministic across substrate restarts.
        let origin = CardOrigin::External {
            source: ExternalSource::GhIssue,
            foreign_id: "1142".into(),
        };
        let json = serde_json::to_string(&origin).expect("serialize");
        let parsed: CardOrigin = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, origin);
    }

    #[test]
    fn external_source_variants_are_named_explicitly() {
        // what this catches: regression where someone replaces the
        // typed `ExternalSource` enum with a free String. Adding a
        // bridge is a deliberate doc + projection change per the
        // [[strong-typing-across-boundaries]] doctrine; a string-typed
        // source defeats that. This test pins the typed shape
        // structurally — the variant set is exhaustive at the type
        // level.
        let sources = [ExternalSource::GhIssue, ExternalSource::GhPullRequest];
        for s in sources {
            let json = serde_json::to_string(&s).expect("serialize ExternalSource");
            let parsed: ExternalSource =
                serde_json::from_str(&json).expect("deserialize ExternalSource");
            assert_eq!(s, parsed);
        }
    }

    #[test]
    fn goal_created_round_trips_with_empty_recipe_refs() {
        // what this catches: regression where `GoalCreated.recipe_refs`
        // loses its `#[serde(default)]` and a legacy event without the
        // field fails to deserialize. Empty Vec at creation is valid
        // per docs; the synthesizer (C3) treats empty as "no synthesis
        // until a recipe is bound."
        let event = GoalCreated {
            goal_id: GoalId::new(),
            title: "ship cross-grid inference".into(),
            default_repo: repo(),
            exit_condition: ExitCondition::AllCardsClosed,
            recipe_refs: vec![],
            created_by: peer(),
            created_at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: GoalCreated = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, event);
    }

    #[test]
    fn goal_created_recipe_refs_serde_default_accepts_legacy_payload() {
        // what this catches: regression where dropping `recipe_refs`
        // from the wire breaks legacy-event decode. The
        // `#[serde(default)]` lets old `GoalCreated` payloads (which
        // never carried `recipe_refs`) project as empty rather than
        // failing the replay. Same convention as `CardCreated.reviews`
        // in event.rs.
        let json = r#"{
            "goal_id": "00000000-0000-0000-0000-000000000001",
            "title": "x",
            "default_repo": "CambrianTech/airc",
            "exit_condition": {"kind": "operator_only"},
            "created_by": "00000000-0000-0000-0000-000000000002",
            "created_at_ms": 1234
        }"#;
        let parsed: GoalCreated =
            serde_json::from_str(json).expect("legacy payload decodes with serde default");
        assert!(parsed.recipe_refs.is_empty());
    }

    #[test]
    fn goal_created_round_trips_with_recipe_refs() {
        // what this catches: regression where `recipe_refs` field
        // ordering / serde alias / RecipeRef-transparent-encoding
        // drifts. The synthesizer's per-goal dispatch (C3) keys off
        // this field; wire drift breaks dispatch.
        let event = GoalCreated {
            goal_id: GoalId::new(),
            title: "x".into(),
            default_repo: repo(),
            exit_condition: ExitCondition::OperatorOnly,
            recipe_refs: vec![
                RecipeRef::new("slice-progression"),
                RecipeRef::new("follow-up-extraction"),
            ],
            created_by: peer(),
            created_at_ms: 0,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: GoalCreated = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, event);
    }

    #[test]
    fn goal_achieved_round_trips_per_condition_variant() {
        // what this catches: regression where the `condition` field's
        // shape diverges from the source `ExitCondition` enum. The
        // projection records WHICH condition fired in the audit trail;
        // both must share the same wire shape so any replayer can
        // reconcile.
        let conditions = [
            ExitCondition::DryForTicks { n: 3 },
            ExitCondition::MilestoneClosed {
                card_id: WorkCardId::new(),
            },
            ExitCondition::AllCardsClosed,
            // OperatorOnly intentionally omitted: the doc says
            // `condition` is never `OperatorOnly` for `GoalAchieved`
            // (operator-only goals transition via a manual path that
            // doesn't emit this event). The type system can't enforce
            // that since `ExitCondition` is the source enum; the
            // invariant is doc'd and the projection enforces it.
        ];
        for c in conditions {
            let event = GoalAchieved {
                goal_id: GoalId::new(),
                condition: c.clone(),
                achieved_at_ms: 999,
            };
            let json = serde_json::to_string(&event).expect("serialize");
            let parsed: GoalAchieved = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, event);
        }
    }

    #[test]
    fn goal_abandoned_round_trips() {
        // what this catches: regression where `reason` becomes
        // optional or `abandoned_by` drops. The audit trail needs
        // both to answer "who killed this goal and why?".
        let event = GoalAbandoned {
            goal_id: GoalId::new(),
            abandoned_by: peer(),
            reason: "scope cut after Q3 reprioritization".into(),
            abandoned_at_ms: 1234,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: GoalAbandoned = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, event);
    }

    #[test]
    fn goal_dry_tick_recorded_round_trips() {
        // what this catches: regression where `GoalDryTickRecorded`
        // loses any field. The projection counts consecutive instances
        // per `goal_id`; a wire-format break breaks the
        // `ExitCondition::DryForTicks` evaluator entirely (v2 residual
        // 1 fix relies on this event landing structurally).
        let event = GoalDryTickRecorded {
            goal_id: GoalId::new(),
            synthesizer_peer: peer(),
            recorded_at_ms: 5678,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: GoalDryTickRecorded = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, event);
    }
}
