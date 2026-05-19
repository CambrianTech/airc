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

    /// Print the default subscribed channel.
    DefaultChannel {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Print the gist id mapped to a channel.
    GetChannelGist {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Channel name.
        #[arg(long)]
        channel: String,
    },

    /// Print channel-to-gist mappings as tab-separated lines.
    ListChannelGists {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
