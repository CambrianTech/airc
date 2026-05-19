use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct MessageArgs {
    #[command(subcommand)]
    pub action: MessageAction,
}

#[derive(Debug, Subcommand)]
pub enum MessageAction {
    /// Build a legacy messages.jsonl chat envelope as JSON.
    BuildLegacy {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        ts: String,
        #[arg(long)]
        channel: String,
        #[arg(long)]
        msg: String,
        #[arg(long, default_value = "")]
        client_id: String,
        #[arg(long, default_value = "")]
        kind: String,
    },
}
