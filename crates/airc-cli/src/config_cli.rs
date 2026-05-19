use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print a config value, or a default when missing/empty.
    Get {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Config key.
        key: String,
        /// Value printed when the key is missing or empty.
        #[arg(default_value = "")]
        default: String,
    },

    /// Print the configured identity name.
    GetName {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Set a config string value.
    Set {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Config key.
        #[arg(long)]
        key: String,
        /// Config value. Empty values are valid.
        #[arg(long, default_value = "")]
        value: String,
    },

    /// Set the configured identity name.
    SetName {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Identity name.
        #[arg(long)]
        name: String,
    },

    /// Remove config keys.
    UnsetKeys {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Keys to remove.
        keys: Vec<String>,
    },

    /// Print parted rooms, one per line.
    ReadParted {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Add a room to parted_rooms.
    RecordParted {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Room name.
        #[arg(long)]
        room: String,
    },

    /// Remove a room from parted_rooms.
    ClearParted {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Room name.
        #[arg(long)]
        room: String,
    },

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

    /// Add a channel to subscribed_channels.
    Subscribe {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Channel name.
        #[arg(long)]
        channel: String,
        /// Promote this channel to the first/default slot.
        #[arg(long)]
        first: bool,
    },

    /// Remove a channel from subscribed_channels.
    Unsubscribe {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Channel name.
        #[arg(long)]
        channel: String,
    },

    /// Set or clear a channel_gists mapping.
    SetChannelGist {
        /// Config file. Defaults to `<home>/config.json`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Channel name.
        #[arg(long)]
        channel: String,
        /// Gist id. Empty clears the mapping.
        #[arg(long, default_value = "")]
        gist_id: String,
    },
}
