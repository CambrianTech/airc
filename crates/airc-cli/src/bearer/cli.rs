use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct BearerArgs {
    #[command(subcommand)]
    pub action: BearerAction,
}

#[derive(Debug, Subcommand)]
pub enum BearerAction {
    /// Deliver one legacy JSONL envelope from stdin.
    Send {
        peer_id: String,
        channel: String,
        #[arg(long)]
        host_target: Option<String>,
        #[arg(long)]
        identity_key: Option<String>,
        #[arg(long)]
        remote_home: Option<String>,
        #[arg(long)]
        room_gist_id: Option<String>,
    },

    /// Deliver many legacy JSONL envelopes from stdin in one transport write.
    SendBatch {
        peer_id: String,
        channel: String,
        #[arg(long)]
        host_target: Option<String>,
        #[arg(long)]
        identity_key: Option<String>,
        #[arg(long)]
        remote_home: Option<String>,
        #[arg(long)]
        room_gist_id: Option<String>,
    },

    /// Stream legacy JSONL envelopes to stdout.
    Recv {
        peer_id: String,
        #[arg(long)]
        host_target: Option<String>,
        #[arg(long)]
        identity_key: Option<String>,
        #[arg(long)]
        remote_home: Option<String>,
        #[arg(long)]
        offset_file: Option<std::path::PathBuf>,
        #[arg(long)]
        state_file: Option<std::path::PathBuf>,
        #[arg(long)]
        room_gist_id: Option<String>,
    },
}
