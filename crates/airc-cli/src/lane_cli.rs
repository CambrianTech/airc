//! Clap argument shapes for `airc-rs lane ...`.

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub struct LaneArgs {
    #[command(subcommand)]
    pub action: LaneAction,
}

#[derive(Debug, Subcommand)]
pub enum LaneAction {
    /// Create a typed work lane in the current room.
    Create {
        /// Repository key, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: String,
        /// Human-readable lane title.
        #[arg(long)]
        title: String,
        /// Initial lane state.
        #[arg(long, value_enum, default_value = "planned")]
        state: CliLaneState,
    },
    /// Change a lane state.
    State {
        /// Lane UUID.
        lane_id: String,
        /// New lane state.
        #[arg(value_enum)]
        state: CliLaneState,
    },
    /// Print the current room's lane projection.
    Status {
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 128)]
        limit: usize,
    },
    /// Claim, release, or inspect the manager hat for a repo.
    Manager {
        #[command(subcommand)]
        action: LaneManagerAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum LaneManagerAction {
    /// Claim the manager hat lease for a repo.
    Claim {
        /// Repository key, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: String,
        /// Lease duration in milliseconds.
        #[arg(long, default_value_t = 900_000)]
        ttl_ms: u64,
    },
    /// Release this peer's manager hat for a repo.
    Release {
        /// Repository key, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: String,
    },
    /// Print current manager hats from the board projection.
    Status {
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 128)]
        limit: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CliLaneState {
    Planned,
    Active,
    Blocked,
    Landing,
    Done,
}
