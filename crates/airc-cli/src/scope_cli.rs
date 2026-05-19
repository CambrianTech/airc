use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct ScopeArgs {
    #[command(subcommand)]
    pub action: ScopeAction,
}

#[derive(Debug, Subcommand)]
pub enum ScopeAction {
    /// Rebuild config.json from durable local scope state.
    RepairConfig {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value = "")]
        default_name: String,
        #[arg(long, default_value = "")]
        host: String,
    },
}
