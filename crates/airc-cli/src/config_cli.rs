use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print subscribed channels, one per line.
    ReadChannels {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
