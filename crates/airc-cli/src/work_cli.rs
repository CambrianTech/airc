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
    /// Claim an existing work card for this peer.
    Claim {
        /// Work card UUID.
        card_id: String,
        /// Claim lease duration.
        #[arg(long, default_value_t = 600_000)]
        ttl_ms: u64,
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
    Release {
        /// Work card UUID.
        card_id: String,
        /// Claim UUID returned by `work claim`.
        claim_id: String,
        /// Optional release reason.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Print the current room's projected work board.
    Board {
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 128)]
        limit: usize,
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
