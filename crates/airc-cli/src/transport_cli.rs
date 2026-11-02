use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct TransportArgs {
    #[command(subcommand)]
    pub action: TransportAction,
}

#[derive(Debug, Subcommand)]
pub enum TransportAction {
    /// Inspect daemon route and delivery observations without refreshing discovery.
    Health {
        /// Suppress output; exit code still reports degraded health.
        #[arg(long)]
        quiet: bool,

        /// Print only warnings, including missing or stale observations.
        #[arg(long)]
        degraded_only: bool,

        /// Exit non-zero when degraded. Intended for scripts.
        #[arg(long)]
        fail: bool,

        /// Emit a single line of structured JSON instead of prose.
        /// Shape: `{"requires_attention": bool, "findings": [{"status": ...,
        /// "check": ..., "detail": ..., "fix": ...}]}`.
        #[arg(long)]
        json: bool,
    },
}
