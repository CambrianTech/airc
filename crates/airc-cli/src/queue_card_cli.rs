use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct QueueCardArgs {
    #[command(subcommand)]
    pub action: QueueCardAction,
}

#[derive(Debug, Subcommand)]
pub enum QueueCardAction {
    /// Build a queue-card issue body from typed fields.
    Body {
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "")]
        branch: String,
        #[arg(long, default_value = "")]
        owner: String,
        #[arg(long, default_value = "")]
        status: String,
        #[arg(long, default_value = "")]
        blockers: String,
        #[arg(long = "env", default_value = "")]
        environment: String,
        #[arg(long, default_value = "")]
        evidence: String,
        #[arg(long, default_value = "")]
        next_action: String,
        #[arg(long, default_value = "")]
        last_heartbeat: String,
    },

    /// Apply set/clear mutations to a queue-card body.
    MutateBody {
        #[arg(long)]
        body_file: PathBuf,
        #[arg(long)]
        mutations_file: PathBuf,
        #[arg(long)]
        log_msg: String,
        #[arg(long)]
        timestamp: String,
    },

    /// Print owner and status from a queue-card body, one per line.
    ClaimFields {
        #[arg(long)]
        body_file: PathBuf,
    },

    /// Build the hand-out message for the top queue candidate.
    DispatchMessage {
        #[arg(long)]
        target_agent: String,
        #[arg(long, default_value = "")]
        extra_message: String,
        #[arg(long)]
        next_json_file: PathBuf,
    },

    /// Append a queue-card body to an existing issue body.
    AdoptBody {
        #[arg(long)]
        issue_json_file: PathBuf,
        #[arg(long)]
        queue_body_file: PathBuf,
        #[arg(long)]
        force: bool,
    },

    /// Summarize queue cards for repo-nudge text.
    NudgeSummary {
        #[arg(long)]
        raw_json_file: PathBuf,
    },

    /// Print title/status/owner/branch from one issue JSON blob.
    NudgeCardMeta {
        #[arg(long)]
        issue_file: PathBuf,
    },

    /// Render and optionally filter open queue cards.
    List {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "")]
        owner: String,
        #[arg(long, default_value = "")]
        status: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        raw_json_file: PathBuf,
    },

    /// Render stale owned queue cards.
    Stale {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "30m")]
        stale_after: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        raw_json_file: PathBuf,
    },

    /// Rank claimable next queue cards for an owner.
    Next {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value = "canary")]
        base: String,
        #[arg(long, default_value = "")]
        repo_root: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        raw_json_file: PathBuf,
    },

    /// Summarize repo-nudge pong replies from queue cards and message log.
    Pongs {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "")]
        sweep_id: String,
        #[arg(long, default_value = "30m")]
        since: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        cards_file: PathBuf,
        #[arg(long)]
        messages_file: PathBuf,
    },

    /// Summarize queue owners, recent activity, and stale claimed cards.
    Availability {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        sweep_id: String,
        #[arg(long, default_value = "30m")]
        since: String,
        #[arg(long, default_value = "30m")]
        stale_after: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        cards_file: PathBuf,
        #[arg(long)]
        messages_file: PathBuf,
    },
}
