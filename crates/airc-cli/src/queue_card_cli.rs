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

    /// Extract PR refs from review-status queue cards for staleness sweeps.
    ReviewRefs {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        raw_json_file: PathBuf,
    },

    /// Print base/head/url metadata from gh pr view JSON.
    PrMeta {
        #[arg(long)]
        pr_file: PathBuf,
    },

    /// Analyze a PR branch for current-base lines it would erase.
    StalenessAnalyze {
        #[arg(long)]
        repo_root: PathBuf,
        #[arg(long, default_value = "")]
        pr_repo: String,
        #[arg(long, default_value = "")]
        pr_num: String,
        #[arg(long)]
        base_ref: String,
        #[arg(long)]
        head_ref: String,
        #[arg(long)]
        base_git_ref: String,
        #[arg(long)]
        head_git_ref: String,
        #[arg(long)]
        merge_base: String,
        #[arg(long, default_value = "")]
        pr_url: String,
        #[arg(long, default_value_t = 40)]
        limit_lines: usize,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        files_file: PathBuf,
        #[arg(long)]
        diff_file: PathBuf,
        #[arg(long)]
        base_new_file: PathBuf,
    },

    /// Print close-merged metadata from gh pr view JSON.
    CloseMergedMeta {
        #[arg(long)]
        pr_file: PathBuf,
    },

    /// Extract close-merged queue refs from gh pr view JSON.
    CloseMergedRefs {
        #[arg(long)]
        pr_file: PathBuf,
        #[arg(long)]
        repo: String,
    },

    /// Print queue-card status from an issue body file.
    CardStatus {
        #[arg(long)]
        body_file: PathBuf,
    },
}
