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

        /// Emit a single line of structured JSON instead of prose.
        /// Shape: `{"verdict": {"kind": "ok|degraded|no-routes", ...},
        /// "endpoints": N, "lan_peers": N, "samples": [...]}`.
        #[arg(long)]
        json: bool,
    },
}
