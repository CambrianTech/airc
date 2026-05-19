use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct ChannelGistArgs {
    #[command(subcommand)]
    pub action: ChannelGistAction,
}

#[derive(Debug, Subcommand)]
pub enum ChannelGistAction {
    /// Find an existing channel gist; never creates.
    Find {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        require_invite: bool,
    },

    /// Host bootstrap decision: existing gist, blocked discovery, or create.
    HostPreflight {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
