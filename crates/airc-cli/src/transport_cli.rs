use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct TransportArgs {
    #[command(subcommand)]
    pub action: TransportAction,
}

#[derive(Debug, Subcommand)]
pub enum TransportAction {
    /// Inspect substrate route health.
    Health {
        /// Suppress output; exit code still reports degraded health.
        #[arg(long)]
        quiet: bool,

        /// Print only degraded route rows.
        #[arg(long)]
        degraded_only: bool,

        /// Exit non-zero when degraded. Intended for scripts.
        #[arg(long)]
        fail: bool,
    },
}
