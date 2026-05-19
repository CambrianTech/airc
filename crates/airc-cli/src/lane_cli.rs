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
