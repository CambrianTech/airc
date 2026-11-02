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

    /// Render the cohesive queue plan from gh issue-list JSON.
    Plan {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value = "30m")]
        stale_after: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        raw_json_file: PathBuf,
    },

    /// Render the read-only queue steward recommendations.
    Steward {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value = "30m")]
        stale_after: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        raw_json_file: PathBuf,
    },
}

impl QueueCardArgs {
    /// Dispatch `airc queue-card ...` (moved from `main.rs`, unchanged).
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            queue_card_commands, queue_card_plan, queue_card_projection, queue_card_runtime,
            queue_card_staleness,
        };
        match self.action {
            QueueCardAction::Body {
                id,
                branch,
                owner,
                status,
                blockers,
                environment,
                evidence,
                next_action,
                last_heartbeat,
            } => queue_card_commands::run_body(queue_card_commands::QueueCardInput {
                id,
                branch,
                owner,
                status,
                blockers,
                environment,
                evidence,
                next_action,
                last_heartbeat,
            }),
            QueueCardAction::MutateBody {
                body_file,
                mutations_file,
                log_msg,
                timestamp,
            } => queue_card_commands::run_mutate_body(
                &body_file,
                &mutations_file,
                &log_msg,
                &timestamp,
            ),
            QueueCardAction::ClaimFields { body_file } => {
                queue_card_commands::run_claim_fields(&body_file)
            }
            QueueCardAction::DispatchMessage {
                target_agent,
                extra_message,
                next_json_file,
            } => queue_card_commands::run_dispatch_message(
                &target_agent,
                &extra_message,
                &next_json_file,
            ),
            QueueCardAction::AdoptBody {
                issue_json_file,
                queue_body_file,
                force,
            } => queue_card_commands::run_adopt_body(&issue_json_file, &queue_body_file, force),
            QueueCardAction::NudgeSummary { raw_json_file } => {
                queue_card_commands::run_nudge_summary(&raw_json_file)
            }
            QueueCardAction::NudgeCardMeta { issue_file } => {
                queue_card_commands::run_nudge_card_meta(&issue_file)
            }
            QueueCardAction::List {
                repo,
                owner,
                status,
                json,
                raw_json_file,
            } => queue_card_projection::run_list(&repo, &owner, &status, json, &raw_json_file),
            QueueCardAction::Stale {
                repo,
                stale_after,
                json,
                raw_json_file,
            } => queue_card_projection::run_stale(&repo, &stale_after, json, &raw_json_file),
            QueueCardAction::Next {
                repo,
                owner,
                base,
                repo_root,
                json,
                raw_json_file,
            } => queue_card_projection::run_next(
                &repo,
                &owner,
                &base,
                &repo_root,
                json,
                &raw_json_file,
            ),
            QueueCardAction::Pongs {
                repo,
                sweep_id,
                since,
                json,
                cards_file,
                messages_file,
            } => queue_card_runtime::run_pongs(
                &repo,
                &sweep_id,
                &since,
                json,
                &cards_file,
                &messages_file,
            ),
            QueueCardAction::Availability {
                repo,
                sweep_id,
                since,
                stale_after,
                json,
                cards_file,
                messages_file,
            } => queue_card_runtime::run_availability(
                &repo,
                &sweep_id,
                &since,
                &stale_after,
                json,
                &cards_file,
                &messages_file,
            ),
            QueueCardAction::ReviewRefs {
                repo,
                raw_json_file,
            } => queue_card_staleness::run_review_refs(&repo, &raw_json_file),
            QueueCardAction::PrMeta { pr_file } => queue_card_staleness::run_pr_meta(&pr_file),
            QueueCardAction::StalenessAnalyze {
                repo_root,
                pr_repo,
                pr_num,
                base_ref,
                head_ref,
                base_git_ref,
                head_git_ref,
                merge_base,
                pr_url,
                limit_lines,
                json,
                files_file,
                diff_file,
                base_new_file,
            } => queue_card_staleness::run_staleness_analyze(
                queue_card_staleness::StalenessAnalyzeInput {
                    repo_root: &repo_root,
                    pr_repo: &pr_repo,
                    pr_num: &pr_num,
                    base_ref: &base_ref,
                    head_ref: &head_ref,
                    base_git_ref: &base_git_ref,
                    head_git_ref: &head_git_ref,
                    merge_base: &merge_base,
                    pr_url: &pr_url,
                    limit: limit_lines,
                    output_json: json,
                    files_file: &files_file,
                    diff_file: &diff_file,
                    base_new_file: &base_new_file,
                },
            ),
            QueueCardAction::CloseMergedMeta { pr_file } => {
                queue_card_commands::run_close_merged_meta(&pr_file)
            }
            QueueCardAction::CloseMergedRefs { pr_file, repo } => {
                queue_card_commands::run_close_merged_refs(&pr_file, &repo)
            }
            QueueCardAction::CardStatus { body_file } => {
                queue_card_commands::run_card_status(&body_file)
            }
            QueueCardAction::Plan {
                repo,
                owner,
                stale_after,
                json,
                raw_json_file,
            } => queue_card_plan::run_plan(&repo, &owner, &stale_after, json, &raw_json_file),
            QueueCardAction::Steward {
                repo,
                owner,
                stale_after,
                json,
                raw_json_file,
            } => queue_card_plan::run_steward(&repo, &owner, &stale_after, json, &raw_json_file),
        }
    }
}
