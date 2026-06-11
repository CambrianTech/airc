//! Pure functions from `(goal_state, board_snapshot)` to next-step
//! proposals (card e4cad280, slice B).
//!
//! A `Recipe` runs against a `RecipeContext` and returns a
//! `Vec<NextStepProposal>` — the idle-agent engine's strategy layer.
//! Recipes are stateless code; all cleverness lives in the recipe's
//! `propose` function. The synthesizer (slice C) dispatches every
//! registered recipe and dedups the merged proposal stream.
//!
//! Per the v2 design (PR #1123 verdict residual 2/3), this crate ships
//! the *seam* but no semantic recipes. Domain vocabulary
//! (review-coverage, follow-up extraction, etc.) lives consumer-side
//! and registers via `ConsumerAdapter` (card 9c63f3d8). The shipped
//! `RecipeRegistry::new()` is empty by construction; an empty registry
//! is a valid runtime state — `IdleVerdict::SynthesizeNow` just produces
//! zero proposals and the engine continues.
//!
//! ## Layering: where the adapter→registry glue lives
//!
//! `airc-lib` depends on `airc-work`, not the other way around — the
//! `ConsumerAdapter` trait (9c63f3d8) is therefore unreachable from this
//! module by construction. `RecipeRegistry::register` takes a bare
//! `Arc<dyn Recipe>` because that's the layering-correct seam: the
//! adapter→registry glue (translate a `ConsumerAdapter` install event
//! into a `register` call) lives in `airc-lib` and gets wired at the
//! `idle_tick` ServiceModule boot site (slice E). Slice C uses this
//! registry directly via `propose_all` without going through any
//! adapter abstraction.
//!
//! ## Doctrines
//!
//! - `[[strong-typing-across-boundaries]]` — `RecipeRef` is a typed
//!   newtype, not a free `String`; `Recipe::propose` returns
//!   `Vec<NextStepProposal>`, not `Vec<serde_json::Value>`.
//! - `[[no-fallbacks-ever]]` — there's no "default recipe" that runs
//!   when none are registered. An empty registry produces empty
//!   proposals; the synthesizer treats that as honest silence, not as
//!   a missing-arm fallback.
//! - **Recipe purity** — `propose` takes `&RecipeContext` and returns
//!   `Vec<NextStepProposal>` synchronously. No `&mut self`, no async,
//!   no `Result<_>` — recipes that can't propose return an empty Vec.
//!   The substrate's audit-trail story (`CardOrigin::Synthesized.recipe_id`)
//!   relies on `propose` being trivially testable + reproducible given
//!   the same inputs.

use std::collections::HashMap;
use std::sync::Arc;

use airc_core::PeerId;
use serde::{Deserialize, Serialize};

use crate::goal::Goal;
use crate::ids::{LaneId, RepoId, WorkCardId};
use crate::model::Priority;
use crate::projection::BoardSnapshot;

/// Stable identifier for a recipe. Used by `CardCreated.origin`
/// (v2 A1) to attribute every synthesized card to the recipe that
/// proposed it — the audit trail's "why does this card exist?" field.
///
/// Free-form string newtype rather than UUID: recipes are human-named
/// (`"slice-progression"`, `"follow-up-extraction"`) and the same name
/// should reference the same recipe across grids that install the same
/// `ConsumerAdapter` (card 9c63f3d8). UUIDs would force a registry
/// lookup just to identify what wrote the card.
///
/// The newtype prevents accidentally substituting an arbitrary string
/// for a recipe identifier per `[[strong-typing-across-boundaries]]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecipeRef(String);

impl RecipeRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecipeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Input handed to every recipe's `propose` call. Pure data; the
/// substrate guarantees `now_ms` is injected (no `Instant::now()` per
/// `[[concurrency-style-guide]]` — recipes must be reproducible against
/// captured snapshots).
///
/// Field set matches the v2 design memo (IDLE-AGENT-ENGINE.md line 109)
/// verbatim — `{goal, board, my_peer_id, now_ms}`. Adding a field later
/// would break every consumer `Recipe` impl's struct-literal call site,
/// so the shape is fixed at slice B even though `my_peer_id` has no
/// in-tree consumer until slice C wires the synthesizer.
pub struct RecipeContext<'a> {
    /// The goal this dispatch is proposing against. Recipes inspect
    /// `goal.state` to decide what to propose (or whether to propose
    /// at all — an `Achieved`/`Abandoned` goal is filtered before
    /// `propose` is called; recipes don't need to re-check).
    pub goal: &'a Goal,
    /// Current open-world board. Recipes read open cards / lane state /
    /// PR state from here. The synthesizer passes the same snapshot
    /// to every recipe in one dispatch so cross-recipe coherence is
    /// guaranteed.
    pub board: &'a BoardSnapshot,
    /// The peer running this synthesis tick. Recipes that need to
    /// distinguish "is this card already claimed by me?" from "is this
    /// card claimed by another peer?" key off this. Slice C's
    /// synthesizer injects the running peer's id.
    pub my_peer_id: PeerId,
    /// Injected wall-clock; substrate doctrine forbids
    /// `Instant::now()` inside hot paths and recipes are particularly
    /// sensitive to this — replay-from-event-log expects deterministic
    /// outputs given the inputs.
    pub now_ms: u64,
}

/// A recipe's proposal for one new card. The synthesizer (slice C)
/// dedups proposals across recipes by `dedup_key` and emits one
/// `CardCreated` per surviving proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextStepProposal {
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub repo: RepoId,
    pub lane_id: Option<LaneId>,
    /// Cards this proposal structurally depends on. The synthesizer
    /// creates the parent cards first and `depends_on` becomes part of
    /// the typed dependency link.
    pub depends_on: Vec<WorkCardId>,
    /// Recipe-provided equivalence key. The recipe is the authority
    /// on "this proposal is the same as a previous one for the same
    /// goal" — substring-matching titles is the wrong shape, so the
    /// recipe declares the relation. Per v2 A4a, the synthesizer's
    /// local dedup is an optimization; correctness is in the
    /// projection's first-write-wins arbitration (slice C).
    pub dedup_key: String,
}

/// A recipe — pure function from `(goal, board, now_ms)` to proposals.
///
/// Implementations must be `Send + Sync` so the registry holds them
/// behind `Arc<dyn Recipe>`. `propose` is sync; if a recipe needs
/// off-process data it must be precomputed and threaded through
/// `BoardSnapshot` (e.g. PR review states already land there via
/// `pull_requests::PullRequestSnapshot`).
pub trait Recipe: Send + Sync {
    /// Stable identifier the synthesizer stamps into
    /// `CardOrigin::Synthesized.recipe_id`.
    fn id(&self) -> &RecipeRef;

    /// Human-readable name for board / CLI output. Distinct from
    /// `id()` because the wire-stable identifier and the operator-facing
    /// label are different concerns; the registry indexes on `id()`.
    fn name(&self) -> &str;

    /// Run the recipe against `ctx` and return any proposals. Empty
    /// Vec is a perfectly valid return value — most ticks for most
    /// recipes will be empty.
    fn propose(&self, ctx: &RecipeContext) -> Vec<NextStepProposal>;
}

/// Pluggable registry of recipes the synthesizer dispatches.
///
/// Empty by default. Consumers (continuum's Sentinel Engine,
/// hermes, openclaw, etc.) `register` their recipes via the
/// `ConsumerAdapter` seam (card 9c63f3d8) — airc-work itself ships
/// zero recipes and zero domain vocabulary.
///
/// `register` is idempotent on `RecipeRef`: re-registering the same
/// id REPLACES the previous recipe. This is deliberately permissive
/// — the typical replacement reason is hot-reloading a consumer
/// adapter — but means production callers shouldn't rely on
/// "first-write-wins at registration time." Per `[[no-fallbacks-ever]]`,
/// the replacement is loud-by-default: callers get an `Option`
/// containing the displaced recipe (so logs can name what was
/// replaced).
#[derive(Clone, Default)]
pub struct RecipeRegistry {
    inner: HashMap<RecipeRef, Arc<dyn Recipe>>,
}

impl RecipeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a recipe. Returns the previously-registered recipe
    /// with the same `RecipeRef`, if any (so callers can log the
    /// replacement explicitly).
    ///
    /// `#[must_use]` is what makes the "replacement is loud" doc claim
    /// real: `registry.register(r);` is a compile-time warning under
    /// the substrate's `-D warnings` gate, so a silent last-wins swap
    /// can't slip through. Callers that genuinely want to discard the
    /// displaced recipe must say so with `let _ = registry.register(r);`.
    #[must_use = "register returns the previously-registered recipe with the same id; \
                  drop explicitly with `let _ = registry.register(r);` to acknowledge \
                  the swap"]
    pub fn register(&mut self, recipe: Arc<dyn Recipe>) -> Option<Arc<dyn Recipe>> {
        let id = recipe.id().clone();
        self.inner.insert(id, recipe)
    }

    /// Number of registered recipes.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Look up a recipe by id. The synthesizer uses this to attribute
    /// proposals back to their source recipe.
    pub fn get(&self, id: &RecipeRef) -> Option<&Arc<dyn Recipe>> {
        self.inner.get(id)
    }

    /// Dispatch every registered recipe against `ctx` and return the
    /// concatenated proposals tagged with their source `RecipeRef`.
    /// Iteration order is unspecified; the synthesizer's projection-
    /// side dedup (slice C, v2 A4a) makes order irrelevant for
    /// correctness.
    pub fn propose_all(&self, ctx: &RecipeContext) -> Vec<(RecipeRef, NextStepProposal)> {
        self.inner
            .values()
            .flat_map(|recipe| {
                let id = recipe.id().clone();
                recipe
                    .propose(ctx)
                    .into_iter()
                    .map(move |p| (id.clone(), p))
            })
            .collect()
    }
}

impl std::fmt::Debug for RecipeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `Arc<dyn Recipe>` values don't impl `Debug`; print the
        // registered RecipeRefs instead so `{:?}` is informative
        // without imposing `Debug` on the trait.
        f.debug_struct("RecipeRegistry")
            .field("registered", &self.inner.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::{ExitCondition, GoalId};
    use crate::ids::RepoId;
    use crate::projection::BoardSnapshot;

    fn repo() -> RepoId {
        RepoId::new("CambrianTech/airc").expect("static repo id")
    }

    fn goal() -> Goal {
        Goal::fresh(
            GoalId::new(),
            "x".into(),
            repo(),
            ExitCondition::OperatorOnly,
            0,
        )
    }

    fn empty_board() -> BoardSnapshot {
        BoardSnapshot {
            cards: vec![],
            lanes: vec![],
            workspaces: vec![],
            repo_tracking: vec![],
            pull_requests: vec![],
            manager_hats: vec![],
            agent_availability: vec![],
            hygiene_reports: vec![],
        }
    }

    /// Recipe that always proposes a fixed card with the given title.
    /// Test fixture only — production recipes register via the
    /// ConsumerAdapter seam.
    struct AlwaysProposes {
        id: RecipeRef,
        title: String,
    }

    impl Recipe for AlwaysProposes {
        fn id(&self) -> &RecipeRef {
            &self.id
        }

        fn name(&self) -> &str {
            "always-proposes (test fixture)"
        }

        fn propose(&self, _ctx: &RecipeContext) -> Vec<NextStepProposal> {
            vec![NextStepProposal {
                title: self.title.clone(),
                body: None,
                priority: Priority::P2,
                repo: repo(),
                lane_id: None,
                depends_on: vec![],
                dedup_key: format!("{}::{}", self.id, self.title),
            }]
        }
    }

    /// Recipe that proposes nothing — most production recipes most
    /// ticks. Pins that an empty return is a valid trait contract.
    struct NeverProposes {
        id: RecipeRef,
    }

    impl Recipe for NeverProposes {
        fn id(&self) -> &RecipeRef {
            &self.id
        }

        fn name(&self) -> &str {
            "never-proposes (test fixture)"
        }

        fn propose(&self, _ctx: &RecipeContext) -> Vec<NextStepProposal> {
            Vec::new()
        }
    }

    #[test]
    fn empty_registry_proposes_nothing() {
        // what this catches: regression where `RecipeRegistry::new`
        // installs a default recipe. Empty must mean empty per
        // [[no-fallbacks-ever]]: an empty registry is honest, not a
        // silent-fallthrough.
        let registry = RecipeRegistry::new();
        let goal = goal();
        let board = empty_board();
        let ctx = RecipeContext {
            goal: &goal,
            board: &board,
            my_peer_id: PeerId::new(),
            now_ms: 0,
        };
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
        assert!(registry.propose_all(&ctx).is_empty());
    }

    #[test]
    fn register_inserts_and_dispatches() {
        // what this catches: regression where `register` silently
        // drops or `propose_all` skips recipes — either breaks the
        // engine's "every registered recipe runs every tick."
        let mut registry = RecipeRegistry::new();
        let displaced = registry.register(Arc::new(AlwaysProposes {
            id: RecipeRef::new("test::always"),
            title: "do the thing".into(),
        }));
        assert!(displaced.is_none(), "fresh registration displaces nothing");
        assert_eq!(registry.len(), 1);

        let goal = goal();
        let board = empty_board();
        let ctx = RecipeContext {
            goal: &goal,
            board: &board,
            my_peer_id: PeerId::new(),
            now_ms: 0,
        };
        let proposals = registry.propose_all(&ctx);
        assert_eq!(proposals.len(), 1);
        let (recipe_id, proposal) = &proposals[0];
        assert_eq!(recipe_id.as_str(), "test::always");
        assert_eq!(proposal.title, "do the thing");
        assert_eq!(proposal.dedup_key, "test::always::do the thing");
    }

    #[test]
    fn register_replaces_same_id_and_returns_displaced() {
        // what this catches: regression where re-registering the same
        // RecipeRef silently inserts both copies (so `propose_all`
        // would emit double proposals) OR silently fails (so a
        // consumer adapter hot-reload leaves the stale recipe live).
        // Per docs: replace is the right semantic; the displaced
        // recipe is returned so logs can name what was replaced.
        let mut registry = RecipeRegistry::new();
        let none = registry.register(Arc::new(AlwaysProposes {
            id: RecipeRef::new("test::dup"),
            title: "v1".into(),
        }));
        assert!(none.is_none(), "first register displaces nothing");
        let displaced = registry.register(Arc::new(AlwaysProposes {
            id: RecipeRef::new("test::dup"),
            title: "v2".into(),
        }));
        assert!(displaced.is_some(), "re-register returns displaced recipe");
        assert_eq!(registry.len(), 1, "no double-insertion");

        let goal = goal();
        let board = empty_board();
        let ctx = RecipeContext {
            goal: &goal,
            board: &board,
            my_peer_id: PeerId::new(),
            now_ms: 0,
        };
        let proposals = registry.propose_all(&ctx);
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].1.title, "v2", "newest registration wins");
    }

    #[test]
    fn never_proposes_recipe_returns_empty_vec() {
        // what this catches: regression where the trait's contract
        // requires non-empty Vec returns (which would force recipes
        // that have nothing to say to invent a no-op proposal — defeats
        // the purpose). An empty return is the most common honest
        // outcome.
        let mut registry = RecipeRegistry::new();
        let _ = registry.register(Arc::new(NeverProposes {
            id: RecipeRef::new("test::never"),
        }));
        let goal = goal();
        let board = empty_board();
        let ctx = RecipeContext {
            goal: &goal,
            board: &board,
            my_peer_id: PeerId::new(),
            now_ms: 0,
        };
        let proposals = registry.propose_all(&ctx);
        assert!(proposals.is_empty());
    }

    #[test]
    fn multiple_recipes_dispatch_independently() {
        // what this catches: regression where one recipe's output
        // shadows another's (e.g. if `propose_all` short-circuits on
        // first non-empty return). Every registered recipe must run
        // every tick; the synthesizer dedups, not the registry.
        //
        // INTERIM SCOPE NOTE: "every registered recipe runs every tick"
        // is the slice-B contract because Goal lacks `recipe_refs`
        // today. Once slice C lands `Goal.recipe_refs: Vec<RecipeRef>`,
        // dispatch will be goal-scoped per the v2 design memo ("runs
        // each goal's recipes", IDLE-AGENT-ENGINE.md §Recipe). The
        // synthesizer will iterate goals and dispatch only the
        // `recipe_refs` registered for each. This test pins the
        // unscoped behavior as load-bearing for the *registry*; the
        // *synthesizer*'s slice-C tests will pin the scoped variant.
        let mut registry = RecipeRegistry::new();
        let _ = registry.register(Arc::new(AlwaysProposes {
            id: RecipeRef::new("test::a"),
            title: "A".into(),
        }));
        let _ = registry.register(Arc::new(AlwaysProposes {
            id: RecipeRef::new("test::b"),
            title: "B".into(),
        }));
        let _ = registry.register(Arc::new(NeverProposes {
            id: RecipeRef::new("test::silent"),
        }));
        assert_eq!(registry.len(), 3);

        let goal = goal();
        let board = empty_board();
        let ctx = RecipeContext {
            goal: &goal,
            board: &board,
            my_peer_id: PeerId::new(),
            now_ms: 0,
        };
        let proposals = registry.propose_all(&ctx);
        // Two recipes proposed one card each; the third stayed silent.
        // Order is unspecified per docs.
        assert_eq!(proposals.len(), 2);
        let titles: std::collections::BTreeSet<_> =
            proposals.iter().map(|(_, p)| p.title.clone()).collect();
        assert_eq!(
            titles,
            ["A".to_string(), "B".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn recipe_ref_round_trips_via_serde() {
        // what this catches: regression where the transparent
        // serialization of `RecipeRef` drifts (e.g. someone adds
        // wrapping serde shape). The wire shape is load-bearing for
        // `CardOrigin::Synthesized.recipe_id` (slice C); breaking it
        // would mean existing cards can't replay.
        let r = RecipeRef::new("follow-up-extraction");
        let json = serde_json::to_string(&r).expect("serialize RecipeRef");
        assert_eq!(json, "\"follow-up-extraction\"", "transparent serde shape");
        let parsed: RecipeRef = serde_json::from_str(&json).expect("deserialize RecipeRef");
        assert_eq!(parsed, r);
    }

    #[test]
    fn next_step_proposal_round_trips_via_serde() {
        // what this catches: regression where `NextStepProposal`'s
        // field set or names drift. The proposal is what becomes
        // `CardCreated` content (slice C), so wire stability matters
        // for replay.
        let proposal = NextStepProposal {
            title: "ship slice C".into(),
            body: Some("synthesizer + projection arbitration".into()),
            priority: Priority::P0,
            repo: repo(),
            lane_id: None,
            depends_on: vec![WorkCardId::new()],
            dedup_key: "e4cad280::slice-c".into(),
        };
        let json = serde_json::to_string(&proposal).expect("serialize");
        let parsed: NextStepProposal = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, proposal);
    }
}
