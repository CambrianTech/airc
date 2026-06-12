//! Clap argument shapes for `airc work ...`.

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub struct WorkArgs {
    #[command(subcommand)]
    pub action: WorkAction,
}

#[derive(Debug, Subcommand)]
pub enum WorkAction {
    /// Create a typed work card in the current room.
    Create {
        /// Repository key, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: String,
        /// Human-readable card title.
        #[arg(long)]
        title: String,
        /// Optional card body.
        #[arg(long)]
        body: Option<String>,
        /// Optional lane UUID to attach this card to.
        #[arg(long)]
        lane_id: Option<String>,
        /// Scheduling priority.
        #[arg(long, value_enum, default_value = "p2")]
        priority: CliPriority,
    },
    /// Idempotently seed a manager/roadmap/RAG candidate into this room.
    Seed {
        /// Repository key, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: String,
        /// Human-readable card title.
        #[arg(long)]
        title: String,
        /// Optional card body.
        #[arg(long)]
        body: Option<String>,
        /// Optional lane UUID to attach this card to.
        #[arg(long)]
        lane_id: Option<String>,
        /// Scheduling priority.
        #[arg(long, value_enum, default_value = "p2")]
        priority: CliPriority,
        /// Stable source key from a roadmap/RAG/issue adapter.
        #[arg(long)]
        evidence_key: Option<String>,
    },
    /// Claim an existing work card for this peer.
    ///
    /// Refuses when the current directory is not under
    /// `~/.airc/worktrees/` (the lease zone). Pass
    /// `--no-lease-required` to override — useful for one-shot
    /// admin claims from the main checkout.
    Claim {
        /// Work card UUID.
        card_id: String,
        /// Claim lease duration.
        #[arg(long, default_value_t = 600_000)]
        ttl_ms: u64,
        /// Allow claim from outside `~/.airc/worktrees/`. Default
        /// behaviour refuses, to keep lane work inside leases.
        #[arg(long)]
        no_lease_required: bool,
    },
    /// Extend this peer's claim lease on a work card.
    Heartbeat {
        /// Work card UUID.
        card_id: String,
        /// Claim UUID returned by `work claim`.
        claim_id: String,
        /// New lease duration from this heartbeat.
        #[arg(long, default_value_t = 600_000)]
        ttl_ms: u64,
    },
    /// Release this peer's claim on a work card.
    ///
    /// `CLAIM_ID` is optional — when omitted, defaults to THIS peer's
    /// active claim on the card (looked up via the board projection).
    /// Pass an explicit `CLAIM_ID` only to release a specific claim
    /// when you have multiple, or to release someone else's claim
    /// (which the daemon will reject if you don't own it).
    ///
    /// Unlike `claim`, `release` does not enforce a `~/.airc/worktrees/`
    /// lease check — an agent must always be able to release a claim it
    /// holds, regardless of cwd. So there is no `--no-lease-required`
    /// flag here; release is unconditionally permitted.
    Release {
        /// Work card UUID.
        card_id: String,
        /// Claim UUID returned by `work claim`. Optional — see command help.
        claim_id: Option<String>,
        /// Optional release reason.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Amend a card's editable fields (title / body / priority) post-creation.
    ///
    /// Card 5ac0a359 — the substrate had no typed way to update a
    /// card after creation, so refining a card's body during
    /// decomposition meant closing + recreating, which broke
    /// references (the new card had a new UUID) and forced every
    /// observer to re-project. `airc work update` emits a typed
    /// `CardUpdated` event whose projection writes only the supplied
    /// fields, leaving everything else (including the card id,
    /// `reviews` link, owner, claim) alone.
    ///
    /// Passing no field flags is legal — it acts as a liveness
    /// marker (bumps `updated_at_ms` without changing semantics).
    /// To clear a body, pass `--body ""` (empty string is the
    /// canonical "no body" idiom for markdown).
    Update {
        /// Work card UUID.
        card_id: String,
        /// New title (omit to leave unchanged).
        #[arg(long)]
        title: Option<String>,
        /// New body (omit to leave unchanged). Pass `""` to clear.
        #[arg(long)]
        body: Option<String>,
        /// New priority (omit to leave unchanged).
        #[arg(long, value_enum)]
        priority: Option<CliPriority>,
    },
    /// Change a work card's lifecycle state.
    State {
        /// Work card UUID.
        card_id: String,
        /// New lifecycle state.
        #[arg(value_enum)]
        state: CliCardState,
    },
    /// Mark a work card closed so it no longer appears as claimable.
    Close {
        /// Work card UUID.
        card_id: String,
    },
    /// Prune worktrees whose work card has reached a terminal state
    /// (Closed or Merged). Card c9b28925: the substrate accumulates a
    /// worktree per claimed card; once the card ships, the worktree
    /// is L1 cache, not durable state ([[local-worktree-is-temp-dir]]).
    /// Default is dry-run (prints what WOULD be removed); pass
    /// `--force` to actually remove. Worktrees with uncommitted /
    /// unpushed changes are NEVER removed silently — the operator's
    /// WIP outranks hygiene.
    Cleanup {
        /// Explicit dry-run flag. Default behaviour even without it,
        /// but useful in scripts so the intent is loud.
        #[arg(long)]
        dry_run: bool,
        /// Actually `git worktree remove` everything classified as
        /// Removable. Dirty / unknown / locked worktrees are still
        /// reported but NEVER touched.
        #[arg(long)]
        force: bool,
    },
    /// Print the current room's projected work board.
    ///
    /// `--available`, `--mine`, `--others` are mutually exclusive filters
    /// over the projection so peers can see their slice fast (kink
    /// b408698c). When none are passed, the full board is shown.
    Board {
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 128)]
        limit: usize,
        /// Show only cards available to claim now: no active claim, or
        /// claim's lease has expired (reclaim-eligible per the
        /// flywheel-continuity doctrine). Closed / Merged are hidden.
        #[arg(long, group = "board_filter")]
        available: bool,
        /// Show only cards currently claimed by this peer.
        #[arg(long, group = "board_filter")]
        mine: bool,
        /// Show only cards currently claimed by another peer.
        #[arg(long, group = "board_filter")]
        others: bool,
    },
    /// Suggest claimable work for this agent.
    Next {
        /// Optional repository filter, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: Option<String>,
        /// Highest priority to include.
        #[arg(long, value_enum, default_value = "p1")]
        max_priority: CliPriority,
        /// Hide expired claims. Normal scheduling treats them as recoverable work.
        #[arg(long)]
        exclude_stale: bool,
        /// Maximum suggestions to print.
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 512)]
        event_limit: usize,
        /// Card e4cad280: idle-signal exit code. With `--check-idle`,
        /// `airc work next` suppresses the human-readable output and
        /// exits with status 0 when there IS claimable work for this
        /// peer, status 1 when the board is idle for this peer.
        /// Useful in agent-loop scripts:
        ///
        ///   while true; do
        ///     if ! airc work next --check-idle; then
        ///       # board is empty — read wall recipes + generate cards
        ///       airc work board --json | jq '...' | generate_recipe_cards
        ///     fi
        ///     airc work next --limit 1 | ...
        ///     sleep 30
        ///   done
        ///
        /// The substrate provides the signal; consumers (Claude tabs,
        /// Codex, hermes/openclaw/continuum agents, personas) decide
        /// what to generate based on wall posts in category="recipe"
        /// or their own goal/state. Card f4227579 / 50a2f7dd /
        /// ea8086e5 (continuum/hermes/openclaw integration) build
        /// the consumer-side generation loops on top of this signal.
        #[arg(long)]
        check_idle: bool,
    },
    /// Show agent liveness, availability, and active work claims.
    Roster {
        /// Optional repository filter, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: Option<String>,
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 512)]
        event_limit: usize,
        /// Heartbeat age to consider live.
        #[arg(long, default_value_t = 180_000)]
        active_within_ms: u64,
    },
    /// Evaluate the typed manager loop: work, roster, and idle-lock cause.
    Manage {
        /// Optional repository filter, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: Option<String>,
        /// Highest priority to include.
        #[arg(long, value_enum, default_value = "p1")]
        max_priority: CliPriority,
        /// Hide expired claims. Normal scheduling treats them as recoverable work.
        #[arg(long)]
        exclude_stale: bool,
        /// Maximum work suggestions to evaluate.
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 512)]
        event_limit: usize,
        /// Heartbeat age to consider live.
        #[arg(long, default_value_t = 180_000)]
        active_within_ms: u64,
    },
    /// Spawn a sibling review card for an existing work card.
    ///
    /// The created card carries a typed `reviews` link to the parent
    /// (card ad7e100b Sub-A: `WorkCard.reviews`), so observers and
    /// schedulers can find every review for a card via
    /// `WorkBoardProjection::review_cards_for(parent_id)` without
    /// parsing body prose.
    ///
    /// Per AGENTS.md §6 every peer has equal authority to review; the
    /// CLI imposes no self-review restriction beyond the agent's own
    /// judgment. If two peers race to spawn a review for the same
    /// parent, both cards land — the projection surfaces them both
    /// (atomic claim arbitrates *who works each*, not whether reviews
    /// exist).
    Review {
        /// Parent card UUID being reviewed.
        parent_id: String,
        /// Optional pull-request URL the reviewer should consult. The
        /// body includes it explicitly so reviewers can find it
        /// without re-projecting the board.
        #[arg(long)]
        pr: Option<String>,
        /// Scheduling priority for the review card. Defaults to
        /// inheriting the parent's priority (the review of a P0 is
        /// itself P0-eligible work).
        #[arg(long, value_enum)]
        priority: Option<CliPriority>,
        /// Optional free-form body — e.g. focus areas or known
        /// gotchas. The parent card id + PR URL are prepended
        /// automatically; this is additive.
        #[arg(long)]
        body: Option<String>,
    },
    /// Publish this agent's availability for a repo.
    Availability {
        /// Repository key, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: String,
        /// Availability state.
        #[arg(long, value_enum)]
        state: CliAvailabilityState,
        /// Optional short note for managers/peers.
        #[arg(long)]
        note: Option<String>,
        /// Availability lease duration.
        #[arg(long, default_value_t = 600_000)]
        ttl_ms: u64,
    },
    /// Card f16650cd: continuous-merge daemon. Long-running process
    /// that polls Review-state cards, merges PRs whose CI is green,
    /// and publishes `PullRequestMerged` so the projection transitions
    /// the card. Runs until Ctrl-C / SIGTERM. One per scope at a time
    /// (`<home>/merger.lock` flock).
    Merger {
        #[command(subcommand)]
        action: MergerAction,
    },
    /// Card a399b342: merge a Review-state card's PR if CI is green.
    ///
    /// The same gate the auto-merger (`airc work merger run`) uses —
    /// strictly-less-red-than-base (card d5b7b07d) — applied to a
    /// one-shot manual merge. Refuses when CI is failing/pending or
    /// the card isn't in Review state with a PR linked.
    ///
    /// Substrate enforcement of "engineering staff" merge discipline:
    /// a less-careful persona reaching for `gh pr merge` directly is
    /// holding the discipline-shaped tool, but `airc work merge`
    /// holds the discipline AND publishes the
    /// `MarkPullRequestMerged` event so the projection transitions
    /// the card consistently with the auto-merger path.
    Merge {
        /// Work card UUID.
        card_id: String,
        /// Print the gate decision (Green / NotReady reason) without
        /// calling `gh pr merge`. Useful before committing.
        #[arg(long)]
        dry_run: bool,
        /// Card 7ed1ac4f: a check pending longer than this is
        /// treated as inherited-from-base ("CI hung, not test
        /// red"). Default 1800s (30 min). Set to 0 to disable the
        /// bypass and require fully-completed CI before merging.
        #[arg(long, default_value_t = 1800)]
        pending_timeout_secs: u64,
    },
    /// Card 70e87d33: retroactively link an already-open PR to a card.
    /// `airc work state review` auto-links PRs it creates, but a PR
    /// opened manually (or before the per-repo base-default fix landed)
    /// has no link, so the merger never sees it. This reads the PR's
    /// head/base from `gh` and emits `PullRequestLinked` so the merger
    /// gate picks it up. Idempotent on an already-linked card.
    Link {
        /// Work card UUID to link the PR to.
        card_id: String,
        /// GitHub PR number to link (e.g. 1471).
        #[arg(long)]
        pr: u64,
    },
    /// Card 09fddedd: re-point a card's linked PR at a successor PR.
    /// `airc work link` is first-write-wins — a card whose round-1 PR
    /// was closed/superseded (orphaned stacked PRs, recovered lanes)
    /// could never track the round-2 PR carrying the real fix, so the
    /// merger skipped the card forever and Review cards orphaned
    /// (failure class: card 6967921d). Relink emits a typed
    /// `PullRequestRelinked` event recording the superseded link for
    /// audit. Refuses loudly when the card has no linked PR (use
    /// `link`), is already Merged/Closed, the successor IS the current
    /// PR, or the successor doesn't exist / targets a different base
    /// than the link it supersedes (validated via `gh --json`).
    Relink {
        /// Work card UUID whose PR link to supersede.
        card_id: String,
        /// Successor PR: a number (e.g. 1137) or a full GitHub PR URL
        /// (e.g. https://github.com/CambrianTech/airc/pull/1137).
        #[arg(long)]
        pr: String,
    },
}

#[derive(Debug, clap::Subcommand)]
pub enum MergerAction {
    /// Start the continuous-merge loop.
    Run {
        /// Poll interval in seconds (default: 30).
        #[arg(long, default_value_t = 30)]
        interval_secs: u64,
        /// Don't actually call `gh pr merge` — log what WOULD be
        /// merged. Useful while validating the gate logic.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CliPriority {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CliAvailabilityState {
    Ready,
    Busy,
    Away,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CliCardState {
    Open,
    Claimed,
    InProgress,
    Blocked,
    Review,
    Merged,
    Closed,
}
