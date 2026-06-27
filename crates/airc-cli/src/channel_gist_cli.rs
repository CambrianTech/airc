use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct ChannelGistArgs {
    #[command(subcommand)]
    pub action: ChannelGistAction,
}

#[derive(Debug, Subcommand)]
pub enum ChannelGistAction {
    /// Resolve a channel gist, optionally creating a new one.
    Resolve {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        create_if_missing: bool,
        #[arg(long)]
        require_invite: bool,
    },

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

    /// Record a newly-created room gist in the local discovery cache.
    RememberCreated {
        #[arg(long)]
        channel: String,
        #[arg(long)]
        gist_id: String,
        #[arg(long)]
        description: String,
        #[arg(long)]
        payload_file: PathBuf,
    },
}
