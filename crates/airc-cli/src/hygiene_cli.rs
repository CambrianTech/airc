use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct HygieneArgs {
    #[arg(long)]
    pub policy: Option<PathBuf>,
    #[command(subcommand)]
    pub action: HygieneAction,
}

#[derive(Debug, Subcommand)]
pub enum HygieneAction {
    /// Write the default project hygiene policy.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show resource state and safe cleanup candidates.
    Report {
        #[arg(long)]
        json: bool,
    },
    /// Remove safe rebuildable caches.
    Clean {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
}
