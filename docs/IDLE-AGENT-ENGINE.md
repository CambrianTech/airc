# Idle-Agent Engine — typed goal/recipe synthesis for the empty-board case

**Status**: Design memo (card e4cad280, claim febc8c42)
**Author**: opus (9bb24964)
**Depends on**: `airc-work` (events, board projection, claim model), `airc-lib` ConsumerAdapter registry (9c63f3d8)
**Related**: card 231bd535 (board/next divergence — board hides claimable cards from some peers; a real upstream cause of "looks idle when it isn't")

---

## Why this exists

The flywheel stops when the board empties. Tonight (2026-06-10/11) the Mac Intel peer went quiet for several /loop heartbeat ticks while the Windows 5090 ran three implementation agents in parallel. Joel's exact words on calling it out: *"their silence doesn't idle this box again."*

The root cause has two layers:

1. **`airc work board` showed 1 card while `airc work next` showed 8 P0s** — substrate gap, carded as 231bd535. Fixing it lifts a class of false-idle.
2. **Even with the board/next gap fixed, a genuinely empty board still strands every peer.** When no card is claimable and no one is producing new ones, every agent goes silent and the flywheel halts. That is e4cad280: the case where idle is *real*, not artifact.

This memo is for case 2: how an idle agent generates the next batch of cards from a goal + recipe, so the board never stays empty when there is still work the goals imply.

---

## The shape

```
   ┌─────────────┐    ┌─────────────┐    ┌─────────────────┐
   │   Goal      │──▶│  Recipe     │──▶│   Synthesizer   │──▶ CardCreated events
   │ (aspiration)│   │ (strategy)  │   │ (idle-tick run) │
   └─────────────┘   └─────────────┘   └─────────────────┘
                                              ▲
                                              │ triggered by
                                              │
                                       ┌──────────────┐
                                       │ IdleDetector │
                                       │ (peer-local) │
                                       └──────────────┘
```

Four typed concerns, one per box, each independently testable:

1. **`Goal`** — a typed long-running aspiration with substate.
2. **`Recipe`** — a pure function from `(goal_state, board_snapshot)` to a `Vec<NextStepProposal>`.
3. **`Synthesizer`** — the seam that runs recipes when the detector fires, dedups proposals against existing board state, and emits `CardCreated` events.
4. **`IdleDetector`** — the peer-local primitive that decides when *I am idle* and the synthesizer should run.

Each box is small, single-purpose, and replaceable. No "master orchestrator," no "intelligent agent runtime." The substrate is small primitives that compose.

---

## The four primitives

### `Goal`

A goal is a long-running aspiration — typed, named, persisted as an `airc-work` event so every peer sees the same goal state.

```rust
pub struct Goal {
    pub id: GoalId,
    pub title: String,
    pub repo: RepoId,          // most goals scope to a repo
    pub state: GoalState,      // typed lifecycle
    pub recipe_refs: Vec<RecipeRef>, // which recipes can synthesize for this goal
    pub created_at_ms: i64,
    pub last_synthesis_at_ms: Option<i64>,
}

pub enum GoalState {
    /// New goal — recipes haven't run yet.
    Fresh,
    /// Recipe has synthesized at least one card; goal is progressing.
    InProgress { open_cards: u32, closed_cards: u32 },
    /// Goal's exit condition is met — recipes refuse to synthesize.
    Achieved { at_ms: i64 },
    /// Goal abandoned by explicit operator action (not by idle inference).
    Abandoned { at_ms: i64, reason: String },
}
```

`GoalState` transitions are events, same shape as `CardStateChanged`. Recipes don't mutate state directly; they emit `GoalProgressed { id, open, closed }` events that the projection applies.

**Examples** that exist today as implicit goals tonight:
- "Land positron substrate end-to-end on canary" (achieved tonight, would be `Achieved`)
- "Cross-grid inference working between Mac and 5090" (card cae4bab1 — would be a `Fresh` goal with a recipe pointing at the cross-grid inference card cluster)
- "Continuum client + desktop UI rework" (5090's full-time lane — `InProgress`)
- "Persona-peer foundation" (5090's other lane — `InProgress`)

### `Recipe`

A recipe is a *pure function* from goal state + board snapshot to next-step proposals. Pure means:
- No side effects
- Reproducible given the same inputs
- Testable in isolation (no daemon, no network)

```rust
pub trait Recipe: Send + Sync {
    /// Stable identifier — used by GoalProgressed events to attribute synthesis.
    fn id(&self) -> &RecipeRef;

    /// Human-readable name for board/CLI output.
    fn name(&self) -> &str;

    /// Run the recipe.
    fn propose(&self, ctx: &RecipeContext) -> Vec<NextStepProposal>;
}

pub struct RecipeContext<'a> {
    pub goal: &'a Goal,
    pub board: &'a BoardSnapshot,         // current open cards + my claims
    pub my_peer_id: PeerId,               // for "what can I claim?" recipes
    pub now_ms: i64,                      // injected, not Instant::now() per substrate doctrine
}

pub struct NextStepProposal {
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub repo: RepoId,
    pub lane_id: Option<LaneId>,
    pub depends_on: Vec<WorkCardId>,      // structural dependency
    pub dedup_key: String,                // recipe-provided; synthesizer rejects duplicates
}
```

**Key shape decisions:**

- **`dedup_key` is recipe-provided, not synthesizer-inferred.** A recipe knows what "the same proposal as last tick" means for its domain; the synthesizer just checks if a card with this dedup_key already exists. Per `[[strong-typing-across-boundaries]]` — substring matching titles is the wrong shape, the recipe declares the equivalence relation.

- **Recipes are pure.** No file I/O, no `gh` calls, no airc subscribes. They get a snapshot, return proposals. This makes them trivially testable + safe to run on every idle tick.

- **`depends_on` is structural.** A recipe that synthesizes a series of slices encodes the dependency in the proposal. The synthesizer creates the cards in dependency order and links them via `CardCreated` event metadata.

**Concrete recipe shapes:**

1. **`SliceProgressionRecipe`** — given a goal with a sequence of named slices ("slice 2A → 2B+2C → 2D-1 → 2D-2"), propose the next slice card when the previous one merges.
2. **`ReviewCoverageRecipe`** — given a goal of "every PR must have ≥1 sentinel verdict," propose review cards for open PRs that lack one.
3. **`FollowupExtractionRecipe`** — given a merged PR with non-blocking findings in its body, propose follow-up cards for each finding. (Closes the "findings ride the PR thread" gap that tonight's #1599 has.)
4. **`RegressionTriageRecipe`** — given a failing CI, propose a card to investigate.

Three of these four shapes are visible in tonight's work without me inventing them. They are the actual flywheel moves we've been doing manually.

### `Synthesizer`

> **⚠️ SUPERSEDED by [v2 A1 + A4](#a1--provenance-is-structural-on-the-card-not-a-side-channel-event).** The v1 design below routes audit attribution through a side-channel `SynthesisRecorded` event and routes goal progress through a count-bearing `GoalProgressed` payload. Both were retired:
>
> - **`SynthesisRecorded` is deleted** — provenance lives structurally on `CardCreated.origin: CardOrigin::Synthesized { goal_id, recipe_id, synthesizer_peer, dedup_key }`. The audit trail rides the primary object; there is no side-channel event to lose or join.
> - **`GoalProgressed` payload counts are deleted** — `GoalState::InProgress { open_cards, closed_cards }` counts are derived by the projection from arbitrated `CardCreated` events keyed by `CardOrigin::Synthesized.goal_id`. The synthesizer doesn't supply pre-arbitration counts.
> - **Dedup arbitration is projection-side** (v2 A4a, first-write-wins on `dedup_key`).
>
> Read the v1 paragraphs below for the dispatch shape; do NOT implement the events/payloads they describe.

The synthesizer is the seam that:

1. On idle-tick: iterates active goals, runs each goal's recipes, collects proposals.
2. Dedups proposals against current board (by `dedup_key`).
3. Emits `CardCreated` events (one per surviving proposal) and a `GoalProgressed` event per goal that had any output.
4. Records a `SynthesisRecorded` event so the audit trail names which recipe synthesized which card.

The synthesizer itself is dumb: it dispatches recipes, dedups, emits. All cleverness is in recipes.

```rust
pub struct Synthesizer<P: PeerSession> {
    pub peer: P,
    pub goals: Arc<dyn GoalStore>,
    pub recipes: RecipeRegistry,
}

impl<P: PeerSession> Synthesizer<P> {
    pub async fn synthesize_once(&self, board: &BoardSnapshot) -> SynthesisOutcome { ... }
}
```

`SynthesisRecorded` is a new event type — emits `(synthesizer_peer, goal_id, recipe_id, card_id, dedup_key)`. The audit trail answers "why does this card exist" without prose.

### `IdleDetector`

The detector is peer-local. It decides when *I* am idle in a way that justifies running the synthesizer.

```rust
pub struct IdleDetector {
    pub my_peer_id: PeerId,
    pub thresholds: IdleThresholds,
}

pub struct IdleThresholds {
    pub min_seconds_since_last_claim_attempt: u64,
    pub min_seconds_since_last_card_created: u64,
    pub require_zero_claimable: bool,   // hard gate: synthesizer only runs if board has nothing for me
    pub max_synthesis_attempts_per_hour: u8, // rate limit; prevents runaway recipe storms
}

impl IdleDetector {
    pub fn is_idle(&self, board: &BoardSnapshot, now_ms: i64) -> IdleVerdict {
        IdleVerdict::*
    }
}

pub enum IdleVerdict {
    /// I am idle by every threshold; synthesizer should run.
    SynthesizeNow,
    /// Board has claimable work I haven't tried — claim first, don't synthesize.
    ClaimableExists { count: u32 },
    /// I tried to claim recently; cool down before synthesizing again.
    Cooldown { until_ms: i64 },
    /// I synthesized too many times this hour; rate-limited.
    RateLimited,
}
```

**The `ClaimableExists` arm is load-bearing for the 231bd535 problem.** With board/next divergence fixed, this arm uses the correct claimable set and never produces a false-idle. Until then, the detector should poll BOTH `board` and `next` and take the union of claimable cards — defensive against the upstream gap.

---

## Idle-tick wiring

The detector and synthesizer compose into one peer-resident tick:

```rust
async fn idle_tick(
    detector: &IdleDetector,
    synthesizer: &Synthesizer,
    peer: &impl PeerSession,
) -> anyhow::Result<()> {
    let board = peer.board_snapshot().await?;
    match detector.is_idle(&board, now_ms()) {
        IdleVerdict::SynthesizeNow => {
            let outcome = synthesizer.synthesize_once(&board).await;
            tracing::info!(
                cards = outcome.cards_proposed,
                dedup_drops = outcome.dedup_drops,
                "synthesized next-step cards",
            );
        }
        IdleVerdict::ClaimableExists { count } => {
            tracing::debug!(count, "skip synthesis — board has claimable work");
        }
        IdleVerdict::Cooldown { until_ms } => {
            tracing::debug!(until_ms, "cooldown after recent claim attempt");
        }
        IdleVerdict::RateLimited => {
            tracing::warn!("idle-tick rate-limited; check for runaway recipes");
        }
    }
    Ok(())
}
```

The tick is cheap when idle isn't fully met (most of the time). Cards only get proposed when no claimable work exists AND cooldown / rate-limit allow.

**Cadence**: per `[[concurrency-style-guide]]` runs as a `ServiceModule` with its own `tokio::time::interval` (every 60-180s — well past the 5-minute cache window). Not a hot path.

---

## What's deliberately NOT in this design

- **No LLM in the synthesizer.** Recipes are pure code. If a recipe wants LLM-assisted synthesis later (e.g. "given this goal's progress notes, propose the next exploration card"), it goes behind an `Adapter` trait and the recipe declares the adapter dependency. The substrate stays composable; LLM use is opt-in per recipe, not a runtime requirement.
- **No "agent personality"** — recipes are typed, named, versioned. Behavior is in code, not vibes. (When persona-resident recipes land later, the personality is in *which* recipes the persona installs, not in the recipe's logic.)
- **No automatic goal creation** — goals are explicit operator-created entities. The synthesizer doesn't invent goals; it acts within existing ones. Same shape as `airc work create` for cards; we add `airc work goal create`.
- **No self-promotion** — the synthesizer can't promote its own cards or claim them. It emits `CardCreated`; existing `airc work claim` flow takes over from there. Claim mechanics stay one place.

---

## Cards this design implies (the first synthesis output)

> **⚠️ SUPERSEDED by [v2's updated slice list](#updated-slice-list).** The list below is the v1 plan. Slice F (in-tree `FollowupExtractionRecipe`) was retired in v2 A2/A3 (semantic recipes move consumer-side); slice C's `SynthesisRecorded` event was retired in v2 A1 (provenance is structural on the card). See [v2 § Updated slice list](#updated-slice-list) for the shipping plan.

If this memo lands, the next batch of cards to build the engine would be:

1. **e4cad280-slice-A** — `Goal` + `GoalStore` event/projection (typed lifecycle, persistence, projection record).
2. **e4cad280-slice-B** — `Recipe` trait + `RecipeRegistry` + `NextStepProposal`.
3. **e4cad280-slice-C** — `Synthesizer` + `SynthesisRecorded` event + dedup gate.
4. **e4cad280-slice-D** — `IdleDetector` + `IdleVerdict` + thresholds.
5. **e4cad280-slice-E** — `idle_tick` ServiceModule + integration test (empty board → recipe runs → cards land).
6. **e4cad280-slice-F** — first concrete recipe: `FollowupExtractionRecipe` (closes the "non-blocking findings ride the PR thread" gap from tonight's #1599).

Each slice is small, independently testable, ships in one PR. Producer-pays sentinel per slice.

---

## Open questions for Fable / 5090

1. **Goal scope**: per-repo, or cross-repo? Cross-grid-inference is implicitly multi-repo (continuum + airc + forge-alloy). Recipes need a way to express "this goal pulls cards across N repos."
2. **Recipe registration**: in-tree static list, or pluggable via `ConsumerAdapter` (9c63f3d8)? Pluggable is more honest but couples to the adapter registry's ship date.
3. **Synthesizer auth**: anyone can synthesize, or only operator-attested goals can? Tentatively: anyone can synthesize for an active goal, but operator-marked goals require operator-signed `GoalActivated` events. Prevents synthesis abuse.
4. **Idle threshold defaults**: 60s? 120s? Should depend on goal cadence (Fresh goal → fast synthesis; InProgress → slower).

---

*Engine built right means the board never stays empty by accident. Joel's directive — "their silence doesn't idle this box again" — is the spec.*

---

## Verdict-driven amendments (v2)

Five amendments resolving sentinel BLOCK findings on v1 (verdict comment 4677441820). Direction was endorsed; the amendments below are structural refinements, not redesign. The earlier sections stay as the design's bones; this section is the diff that ships.

### A1 — Provenance is structural on the card, not a side-channel event

Same erasure shape as positron #1602: the v1 `SynthesisRecorded` side-channel event let `CardCreated` look identical whether it came from an operator or a synthesizer. That's the "first-class but never anonymous" violation transplanted from the wire layer to the work layer.

Fix: `CardCreated` carries a typed `CardOrigin` field. Synthesis provenance is on the card by construction — not reconstructable from a separate event stream.

```rust
pub enum CardOrigin {
    /// Created by a human or persona acting directly (today's default).
    Operator { peer_id: PeerId },
    /// Created by a synthesizer running a recipe against a goal.
    Synthesized {
        goal_id: GoalId,
        recipe_id: RecipeRef,
        synthesizer_peer: PeerId,
        dedup_key: String,        // moves onto the card from NextStepProposal
    },
    /// Created by an external bridge (gh issue mirror, jira import, etc.).
    External { source: ExternalSource, foreign_id: String },
}
```

`CardCreated.origin: CardOrigin` is non-optional. `SynthesisRecorded` is deleted from the design — the audit trail is the `Synthesized` variant on the card itself. Replays remain decisive: every replayed card knows where it came from without needing two events in the right order.

### A2/A3 — Pluggable recipe registry via ConsumerAdapter; semantic recipes live consumer-side

v1's `RecipeContext` correctly fed `SliceProgressionRecipe` and `RegressionTriageRecipe` (both work purely off `BoardSnapshot` shape). But `ReviewCoverageRecipe` ("every PR must have ≥1 sentinel verdict") and `FollowupExtractionRecipe` ("non-blocking findings ride the PR thread") both need *domain* — the sentinel verdict format, the convention for marking findings as "non-blocking" in a PR body. That's continuum/openclaw/hermes-flavored knowledge that doesn't belong in airc.

Fix:

- **airc-work ships the registry seam, not the recipes.** `RecipeRegistry` is a thin `HashMap<RecipeRef, Arc<dyn Recipe>>` with a pluggable `register(adapter)` method that consumes a `ConsumerAdapter` (9c63f3d8) carrying the recipe.
- **The shipped registry is empty by default.** Continuum installs its review-coverage + follow-up-extraction recipes via the adapter seam at boot. openclaw / hermes / future consumers install theirs.
- **Slice F changes scope.** It now ships only the seam wiring (the integration that lets continuum install a recipe), not the recipe content. The continuum-side `FollowupExtractionRecipe` ships in continuum's tree as a follow-up PR there.

This reduces airc's required slice count from 6 to 5 (Goal / Recipe trait + dedup / Synthesizer / IdleDetector / idle_tick + adapter seam) and makes the bd61836b doctrine "domain vocabulary doesn't enter airc" structural.

### A4 — Three runaway holes closed structurally

v1 made dedup, lifecycle, and exit-condition into "the synthesizer does the right thing." All three are easy to get wrong silently. Each gets a typed structural guard:

**A4a — Dedup needs first-write-wins projection arbitration.** v1's "synthesizer checks dedup_key against board" races: two idle peers each pass the dedup check locally, both emit `CardCreated` with the same `dedup_key`, both end up on the board. Fix: `CardCreated` events carry `dedup_key` in their typed body (already in A1's `Synthesized` variant), and the `WorkBoardProjection` applies first-write-wins on `dedup_key` — the second `CardCreated` with a colliding key is recorded as a duplicate (an event the projection emits, useful for telemetry) but doesn't create a card. The synthesizer's local dedup is now an optimization; correctness is in the projection.

**A4b — Closed cards must not re-synthesize.** v1's `Synthesizer::synthesize_once` dedups against the board's *open* cards. If a recipe's `dedup_key` matches a *closed* card, it would synthesize forever — every idle tick spawns a new card, last cycle's card already closed and ignored. Zombie loop. Fix: the dedup pass checks the projection's full `dedup_key` history (open + closed), not just the open set. Closed-with-`dedup_key` is a "this work was already done; don't redo it" signal.

**A4c — Goal exit-condition evaluator.** v1's `Goal::state` transitions through `Achieved` somehow, but nothing said how. Fix: `Goal` carries a typed `ExitCondition` evaluated by the projection on every relevant event:

```rust
pub enum ExitCondition {
    /// Achieved when every recipe yields zero proposals for N consecutive ticks.
    DryForTicks { n: u8 },
    /// Achieved when a specific named milestone card closes.
    MilestoneClosed { card_id: WorkCardId },
    /// Achieved when total open cards for this goal drops to zero (and at least one ever existed).
    AllCardsClosed,
    /// Operator-marked only; no automatic transition.
    OperatorOnly,
}
```

The projection transitions `GoalState::InProgress → Achieved` automatically when the typed condition fires. `OperatorOnly` is the explicit-only path for goals where exit is judgment, not arithmetic. Per `[[no-fallbacks-ever]]`: there's no default-to-OperatorOnly silent path — every `Goal` declares its `ExitCondition` at creation.

### A5 — IdleThresholds inherits the drain-before-produce term

Synthesis MUST NOT outrank reviewing (doctrine 78344eeb). Tonight's review-debt drain proved the cost: the Mac peer went quiet for hours while four PRs sat unreviewed. If `IdleDetector::SynthesizeNow` had fired during that window, the peer would have *produced* new cards instead of *draining* the review queue — exactly the failure mode 78344eeb prohibits.

Fix: `IdleThresholds` grows a `max_unreviewed_assigned_to_me` term; `IdleVerdict` grows a `ReviewDebtFirst` variant:

```rust
pub struct IdleThresholds {
    pub min_seconds_since_last_claim_attempt: u64,
    pub min_seconds_since_last_card_created: u64,
    pub require_zero_claimable: bool,
    pub max_synthesis_attempts_per_hour: u8,
    /// Synthesis is blocked while I have N or more PRs awaiting review that
    /// I'm the producer of (producer-pays sentinel doctrine).
    pub max_unreviewed_assigned_to_me: u8,    // typically 0 — any review debt blocks synthesis
}

pub enum IdleVerdict {
    SynthesizeNow,
    ClaimableExists { count: u32 },
    Cooldown { until_ms: i64 },
    RateLimited,
    /// Drain-before-produce: review queue has PRs I produced; synthesizing would violate 78344eeb.
    ReviewDebtFirst { my_open_review_targets: u32 },
}
```

The `IdleDetector` consults the `BoardSnapshot`'s PR projection (already in `pull_requests/`) for "PRs whose producer is me AND that have no sentinel verdict yet." If that count exceeds `max_unreviewed_assigned_to_me`, no synthesis. Drain first.

This is the structural enforcement of tonight's review-debt drain doctrine. The producer-pays sentinel pass on #1597/#1598/#1599/#1607 happens *because the engine refuses to invent new work while old work sits unreviewed*.

### Updated slice list

The amendments collapse v1's six slices to five and shift Slice F's content to a consumer-side follow-up:

1. **e4cad280-slice-A** — `Goal` + `ExitCondition` enum + `GoalStore` event/projection (lifecycle includes automatic `Achieved` transition via projected evaluator).
2. **e4cad280-slice-B** — `Recipe` trait + `RecipeContext` + `NextStepProposal` + `RecipeRegistry` (empty, pluggable via ConsumerAdapter).
3. **e4cad280-slice-C** — `Synthesizer` + `CardOrigin::Synthesized` carrying `dedup_key` + projection-side first-write-wins dedup arbitration + closed-card dedup check.
4. **e4cad280-slice-D** — `IdleDetector` + `IdleVerdict::ReviewDebtFirst` + `IdleThresholds.max_unreviewed_assigned_to_me`.
5. **e4cad280-slice-E** — `idle_tick` ServiceModule + integration test pinning two invariants: (a) empty board + zero review debt + active goal → recipe runs → card lands with correct `CardOrigin::Synthesized`; (b) two peers' idle ticks producing colliding `dedup_key`s → projection arbitrates first-write-wins, second `CardCreated` recorded as duplicate, no double card.
6. **(Consumer follow-up)** — Continuum installs `FollowupExtractionRecipe` via the adapter seam in a continuum-side PR. Closes the "non-blocking findings ride the PR thread" gap from #1599.

Each slice is small, independently testable, ships in one PR. Producer-pays sentinel per slice — and the engine's own `IdleVerdict::ReviewDebtFirst` enforces it.

