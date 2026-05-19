use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct MonitorArgs {
    #[command(subcommand)]
    pub action: MonitorAction,
}

#[derive(Debug, Subcommand)]
pub enum MonitorAction {
    /// Read legacy JSONL monitor frames from stdin and render stdout notifications.
    Format {
        /// Legacy peers directory containing <peer>.json records.
        #[arg(long)]
        peers_dir: PathBuf,
        /// Current local display name.
        #[arg(long)]
        my_name: String,
    },
}
