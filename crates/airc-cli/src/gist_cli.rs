use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct GistArgs {
    #[command(subcommand)]
    pub action: GistAction,
}

#[derive(Debug, Subcommand)]
pub enum GistAction {
    /// Read a dotted/indexed path from stdin JSON.
    Get {
        path: String,
        #[arg(long, default_value = "")]
        default: String,
    },

    /// Read a dotted/indexed path and emit compact JSON.
    GetJson { path: String },

    /// Read the first present path from stdin JSON.
    GetFirstOf {
        paths: Vec<String>,
        #[arg(long, default_value = "")]
        default: String,
    },

    /// Pick the first address entry matching a scope.
    PickAddr { scope: String },

    /// Pick the first address entry.
    PickAddrFirst,

    /// Pick the first non-localhost address entry.
    PickAddrNonlocalFirst,

    /// Pick the first address entry whose scope is not excluded.
    PickAddrExcluding { exclude_scopes: Vec<String> },

    /// Emit LAN address entries as compact JSONL.
    ListLanEntries,

    /// Extract the selected content file from a GitHub gist API response.
    GistContent {
        #[arg(long, default_value = "")]
        channel: String,
    },

    /// Extract a named file's content from a GitHub gist API response.
    FileContent {
        /// Exact gist filename, for example airc-room-general.json.
        #[arg(long)]
        filename: String,
    },
}
