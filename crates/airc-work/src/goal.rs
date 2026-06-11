//! Long-running aspirations the idle-agent engine synthesizes work
//! against (card e4cad280, slice A).
//!
//! A `Goal` is a typed aspiration that recipes propose `NextStepProposal`s
//! for when the board is empty enough that synthesis is warranted (see
//! `IDLE-AGENT-ENGINE.md` for the full design). This module ships the
//! pure data types — `Goal`, `GoalState`, `ExitCondition`, `GoalId` —
//! and pins the lifecycle invariants at the test level. Events,
//! projection, and the `Achieved` evaluator land in slice C alongside
//! the synthesizer (so the projection has something to apply against),
//! tracked in the verdict's slice-binding notes for PR #1123.
//!
//! ## Doctrines
//!
//! - `[[strong-typing-across-boundaries]]` — `ExitCondition` is a typed
//!   enum, not a heuristic; `GoalState` is a lifecycle, not a string;
//!   `GoalId` is a newtype, not a `Uuid` everyone has to remember to
//!   wrap.
//! - `[[no-fallbacks-ever]]` — every `Goal` declares its `ExitCondition`
//!   at creation. There's no default-to-`OperatorOnly` silent path; if
//!   you want operator-only achievement, you say so explicitly. Same
//!   shape as the substrate's other no-default-arms types.
//! - **Producer-pays sentinel** (78344eeb) — `IdleVerdict::ReviewDebtFirst`
//!   (slice D) blocks synthesis while the producer has unreviewed PRs;
//!   that gate exists upstream of any goal evaluation, so a `Goal` doesn't
//!   need a "should-I-run-now?" predicate.

use serde::{Deserialize, Serialize};

use crate::ids::RepoId;

// `GoalId` lives next to its siblings (`WorkCardId`, `LaneId`, etc.) in
// `crate::ids` so the `uuid_id!` macro stays canonical; the type is
// re-exported here for ergonomic `use crate::goal::*;` imports.
pub use crate::ids::GoalId;

/// A long-running aspiration the idle-agent engine synthesizes work
/// against.
///
/// Goals are explicit operator-created entities. The synthesizer
/// doesn't invent goals; it acts within existing ones. Same shape as
/// `airc work create` for cards; goals get `airc work goal create` (CLI
/// lands in slice E).
///
/// ## Spec fields that don't land here
///
/// The v1 design memo's pseudo-`Goal` carried two fields that this
/// slice deliberately omits:
///
/// - `recipe_refs: Vec<RecipeRef>` — which recipes are eligible to
///   propose for this goal. `RecipeRef` lands in slice B (PR #1125),
///   but the `recipe_refs` field on `Goal` lands in slice C alongside
///   the synthesizer. Rationale: coupling Goal lifecycle (this module)
///   to recipe registration timing belongs at the synthesizer seam
///   where dispatch happens, not here where the lifecycle is pure data.
///   Slice C also wires the projection-side dedup arbitration (v2 A4a)
///   that needs to see `recipe_refs` to scope per-goal dispatch
///   correctly. Slice B's `RecipeRegistry::propose_all` dispatches all
///   registered recipes against any goal as an interim shape; once
///   `recipe_refs` lands in slice C, the synthesizer scopes dispatch
///   per goal per the v2 design memo's "runs each goal's recipes" line.
/// - `last_synthesis_at_ms: Option<u64>` — projection-derived from
///   `CardCreated` events with `CardOrigin::Synthesized.goal_id == self.id`,
///   not stored on the event-sourced goal. The synthesizer reads it via
///   the projection in slice E's `idle_tick`, not from the `Goal` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: GoalId,
    pub title: String,
    /// Goals bind to the BOARD (the room's work projection), not a
    /// single repo. Synthesized cards carry their own `repo` field per
    /// `NextStepProposal`. The `default_repo` here is the repo the
    /// operator created the goal *for* — recipes default new proposals
    /// to it, but a cross-repo goal (e.g. "cross-grid inference"
    /// spanning continuum + airc + forge-alloy) is free to propose
    /// cards in other repos. Resolves PR #1123 verdict's open
    /// question (1) "Goal scope: per-repo, or cross-repo?" with
    /// Fable's answer: cross-repo by default, repo is a field on the
    /// card not a boundary on the goal.
    pub default_repo: RepoId,
    pub state: GoalState,
    /// How the projection (slice C) auto-transitions
    /// `InProgress → Achieved`. Declared at creation; never
    /// `Option::None` per `[[no-fallbacks-ever]]`.
    pub exit_condition: ExitCondition,
    pub created_at_ms: u64,
}

/// Lifecycle of a goal. Transitions are events (slice C) applied by
/// the projection; this enum is the pure data shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalState {
    /// New goal — recipes haven't proposed anything yet.
    Fresh,
    /// At least one card synthesized for this goal is or has been on
    /// the board.
    ///
    /// Loser-side drift guard (verdict 4677490672 residual 3): the
    /// projection (slice C) MUST derive these counts from arbitrated
    /// `CardCreated` events keyed by `CardOrigin::Synthesized.goal_id`,
    /// never from a synthesizer-supplied `GoalProgressed` payload. Under
    /// the v2 design's projection-side first-write-wins dedup (A4a), the
    /// racing-loser peer's local view of `(open_cards, closed_cards)` is
    /// pre-arbitration and would poison the projection if the loser's
    /// payload were trusted. The fields here are projection-only output;
    /// any wire event that carries them is advisory at best.
    InProgress { open_cards: u32, closed_cards: u32 },
    /// `ExitCondition` fired. Recipes refuse to propose; the synthesizer
    /// skips this goal until an operator re-opens it (event TBD,
    /// slice C).
    Achieved { at_ms: u64 },
    /// Operator marked the goal abandoned. Recipes refuse to propose;
    /// same shape as `Achieved` for the synthesizer but distinguishable
    /// for the audit trail.
    Abandoned { at_ms: u64, reason: String },
}

/// How the projection decides a goal has been achieved automatically.
/// Operator-only goals declare `OperatorOnly` and require an explicit
/// event to transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExitCondition {
    /// Achieved when recipes produce zero proposals for `n` consecutive
    /// idle ticks. Per the v2 design memo's slice-binding note: the
    /// synthesizer emits a typed `GoalDryTickRecorded` event the
    /// projection can count, because dry ticks otherwise emit no
    /// events and the projection couldn't observe them. The event +
    /// projection wiring land in slice C.
    DryForTicks { n: u8 },
    /// Achieved when a specific named card closes. Useful for "ship
    /// positron substrate end-to-end on canary" style goals where one
    /// milestone closure is THE achievement.
    MilestoneClosed { card_id: crate::ids::WorkCardId },
    /// Achieved when every card whose `CardOrigin::Synthesized.goal_id`
    /// equals this goal is closed AND at least one such card has ever
    /// existed. The "at least one ever existed" guard prevents a
    /// `Fresh` goal from being auto-`Achieved` because trivially no
    /// open cards exist for it.
    AllCardsClosed,
    /// No automatic transition. Only an explicit `GoalAchieved` event
    /// from an operator (or `GoalAbandoned`) moves the goal out of
    /// `InProgress`. Per `[[no-fallbacks-ever]]`: this is the only
    /// "manual" arm, and it's explicitly named — there's no implicit
    /// fallthrough.
    OperatorOnly,
}

impl Goal {
    /// Construct a `Fresh` goal. The typical creation path —
    /// `airc work goal create` (slice E CLI) calls this then emits
    /// `GoalCreated` (slice C event).
    pub fn fresh(
        id: GoalId,
        title: String,
        default_repo: RepoId,
        exit_condition: ExitCondition,
        created_at_ms: u64,
    ) -> Self {
        Self {
            id,
            title,
            default_repo,
            state: GoalState::Fresh,
            exit_condition,
            created_at_ms,
        }
    }

    /// True iff the goal accepts synthesis right now. The synthesizer
    /// skips goals in `Achieved` / `Abandoned` per the engine design.
    pub fn accepts_synthesis(&self) -> bool {
        matches!(self.state, GoalState::Fresh | GoalState::InProgress { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RepoId;

    fn repo() -> RepoId {
        RepoId::new("CambrianTech/airc").expect("static repo id")
    }

    #[test]
    fn fresh_goal_accepts_synthesis() {
        // what this catches: regression where `Fresh` is incorrectly
        // excluded from `accepts_synthesis`, which would mean a
        // brand-new goal can never have its first card synthesized —
        // the engine would never start producing.
        let g = Goal::fresh(
            GoalId::new(),
            "ship cross-grid inference".into(),
            repo(),
            ExitCondition::AllCardsClosed,
            0,
        );
        assert!(g.accepts_synthesis());
    }

    #[test]
    fn in_progress_goal_accepts_synthesis() {
        // what this catches: regression where `InProgress` stops
        // accepting synthesis, which would mean a goal with a few
        // closed cards stops producing — the flywheel halts when
        // partial progress should keep it moving.
        let g = Goal {
            id: GoalId::new(),
            title: "x".into(),
            default_repo: repo(),
            state: GoalState::InProgress {
                open_cards: 2,
                closed_cards: 3,
            },
            exit_condition: ExitCondition::AllCardsClosed,
            created_at_ms: 0,
        };
        assert!(g.accepts_synthesis());
    }

    #[test]
    fn achieved_goal_refuses_synthesis() {
        // what this catches: regression where an `Achieved` goal keeps
        // accepting synthesis, which would re-spawn the same cards
        // forever (the v2-design zombie loop A4b is supposed to
        // prevent — `accepts_synthesis` is the first gate, dedup
        // history is the second).
        let g = Goal {
            id: GoalId::new(),
            title: "x".into(),
            default_repo: repo(),
            state: GoalState::Achieved { at_ms: 123 },
            exit_condition: ExitCondition::AllCardsClosed,
            created_at_ms: 0,
        };
        assert!(!g.accepts_synthesis());
    }

    #[test]
    fn abandoned_goal_refuses_synthesis() {
        // what this catches: regression where `Abandoned` accepts
        // synthesis, defeating the operator's explicit "stop working
        // on this" signal.
        let g = Goal {
            id: GoalId::new(),
            title: "x".into(),
            default_repo: repo(),
            state: GoalState::Abandoned {
                at_ms: 456,
                reason: "scope cut".into(),
            },
            exit_condition: ExitCondition::AllCardsClosed,
            created_at_ms: 0,
        };
        assert!(!g.accepts_synthesis());
    }

    #[test]
    fn exit_condition_is_required_at_construction() {
        // what this catches: regression where someone adds an
        // `Option<ExitCondition>` field as a "for back-compat" shortcut.
        // Per [[no-fallbacks-ever]], every goal declares its exit
        // condition at creation. The type system enforces it: `Goal`'s
        // `exit_condition` field is non-optional, and `Goal::fresh`'s
        // signature takes it by value. This test pins the contract
        // structurally — if a future refactor relaxes the field to
        // `Option<_>`, this test won't compile, which is the right
        // failure mode.
        let g = Goal::fresh(
            GoalId::new(),
            "x".into(),
            repo(),
            ExitCondition::OperatorOnly,
            0,
        );
        // Take the field by value to assert non-`Option` typing.
        let _: ExitCondition = g.exit_condition;
    }

    #[test]
    fn goal_state_round_trips_via_serde() {
        // what this catches: regression where the `#[serde(tag = "kind")]`
        // representation drifts and `GoalCreated` events on the wire
        // (slice C) decode to a different variant than they encode from.
        // The tag layout IS the wire shape; this test pins it so
        // wire-format changes are deliberate, not accidental.
        let states = [
            GoalState::Fresh,
            GoalState::InProgress {
                open_cards: 3,
                closed_cards: 5,
            },
            GoalState::Achieved { at_ms: 1234 },
            GoalState::Abandoned {
                at_ms: 2345,
                reason: "scope cut".into(),
            },
        ];
        for s in states {
            let json = serde_json::to_string(&s).expect("serialize GoalState");
            let parsed: GoalState = serde_json::from_str(&json).expect("deserialize GoalState");
            assert_eq!(s, parsed, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn exit_condition_round_trips_via_serde() {
        // what this catches: regression where the `ExitCondition` wire
        // shape drifts. Same justification as `goal_state_round_trips_via_serde`
        // but for the exit condition — `GoalCreated` carries this field
        // and a wire-format break here means existing goals can't be
        // replayed.
        let conds = [
            ExitCondition::DryForTicks { n: 3 },
            ExitCondition::MilestoneClosed {
                card_id: crate::ids::WorkCardId::new(),
            },
            ExitCondition::AllCardsClosed,
            ExitCondition::OperatorOnly,
        ];
        for c in conds {
            let json = serde_json::to_string(&c).expect("serialize ExitCondition");
            let parsed: ExitCondition =
                serde_json::from_str(&json).expect("deserialize ExitCondition");
            assert_eq!(c, parsed, "round-trip mismatch for {json}");
        }
    }
}
