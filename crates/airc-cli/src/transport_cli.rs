use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct TransportArgs {
    #[command(subcommand)]
    pub action: TransportAction,
}

#[derive(Debug, Subcommand)]
pub enum TransportAction {
    /// Check scope-local transport heartbeat and pid evidence.
    Health {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,

        /// Maximum acceptable heartbeat age in seconds.
        #[arg(long, default_value_t = 90)]
        fresh_after: u64,

        /// Suppress output; exit code still reports degraded health.
        #[arg(long)]
        quiet: bool,

        /// Print only degraded channel rows.
        #[arg(long)]
        degraded_only: bool,

        /// Exit non-zero when degraded. Intended for scripts.
        #[arg(long)]
        fail: bool,
    },
}
