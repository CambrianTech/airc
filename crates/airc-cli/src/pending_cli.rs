use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct PendingArgs {
    #[command(subcommand)]
    pub action: PendingAction,
}

#[derive(Debug, Subcommand)]
pub enum PendingAction {
    /// Resolve whether a pending snapshot can be sent as one host broadcast batch.
    HostBroadcastRoute {
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value = "")]
        fallback_gist: String,
    },
}
