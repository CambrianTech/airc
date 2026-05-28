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
