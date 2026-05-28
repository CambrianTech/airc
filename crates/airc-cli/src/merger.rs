//! Continuous-merge background loop — auto-merges Review-state cards
//! whose linked PRs have green CI, then publishes `PullRequestMerged`
//! so the projection transitions the card to `Merged`.
//!
//! Card f16650cd. Joel directive 2026-05-28: "we do NOT just leave
//! branches laying around after CI passes" + "we need continuous
//! merging somewhere". Without this, an agent has to remember to run
//! `gh pr merge` — a discipline that breaks the moment a less-capable
//! persona attaches. Substrate-driven; agents go idle, work keeps
//! shipping.
//!
//! ## What this gate checks
//!
//! On every tick (default 30s):
//!
//! 1. Read the work board projection.
//! 2. For every card in `Review` state with a `pull_request`:
//!    - Fetch the PR's check rollup via `gh pr view --json statusCheckRollup`.
//!    - Apply the *strictly-less-red-than-base* doctrine refinement: a PR
//!      is mergeable when no NEW red was introduced vs. the base branch
//!      (the bar agreed during #1033 — pre-existing red doesn't block;
//!      new red does). For the first cut we use a simpler form: no
//!      FAILURE/CANCELLED conclusions in the rollup. The pre-existing-red
//!      bypass is carded as a follow-up.
//!    - If the gate passes: `gh pr merge --squash --delete-branch`.
//!    - On success, publish `MarkPullRequestMerged`. The projection
//!      transitions the card to `Merged`; a separate observer (or the
//!      next merger tick) closes the card.
//!
//! ## What this gate does NOT yet check (future cards)
//!
//! - Peer-LGTM convention (card 267d68f5): for multi-author rooms we
//!   should require a non-author LGTM. For now, ANY Review card
//!   auto-merges if green — appropriate for single-author scopes; for
//!   multi-author, the doctrine says "review before Review state."
//! - Worktree cleanup on merge (card abe9fe4c).
//! - Card-close on merged (currently relies on projection state and an
//!   agent's explicit close; an observer would automate it).
//!
//! ## Singleton enforcement
//!
//! A naive multi-tab launch would race on `gh pr merge` and one would
//! fail with "PR already merged." A file lock at
//! `<home>/merger.lock` prevents two merger processes per scope. The
//! lock is `flock(LOCK_EX | LOCK_NB)` — non-blocking; a second launch
//! exits cleanly with a "merger already running" message.

use std::path::Path;
use std::time::Duration;

use airc_lib::{Airc, MarkPullRequestMerged, WorkCard};

/// Run the continuous-merge loop until shutdown (Ctrl-C / SIGTERM).
/// Default interval at the CLI layer is 30 seconds — CI pipelines on
/// this project take 2–5 minutes; polling faster wastes API calls
/// without shipping more.
///
/// Holds an exclusive file lock at `<home>/merger.lock` so two
/// invocations against the same scope don't race on `gh pr merge`.
pub async fn run(
    home: &Path,
    interval: Duration,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = acquire_singleton_lock(home)?;

    let socket = crate::cli::default_socket_path_in(home);
    crate::commands::ensure_daemon_running(home, socket.clone(), Vec::new()).await?;
    let airc = Airc::attach(home, socket).await?;

    eprintln!(
        "airc-merger: started (home={}, interval={:?}, dry_run={})",
        home.display(),
        interval,
        dry_run
    );
    eprintln!("airc-merger: peer_id={}", airc.peer_id());

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                eprintln!("airc-merger: shutdown signal received, exiting cleanly");
                return Ok(());
            }
            _ = ticker.tick() => {
                if let Err(error) = tick_once(&airc, dry_run).await {
                    // A tick failing should NOT bring down the loop —
                    // gh might be rate-limited, the daemon might be
                    // momentarily unreachable, etc. Log and continue.
                    eprintln!("airc-merger: tick failed: {error}");
                }
            }
        }
    }
}

/// One pass over the board: scan eligible cards, gate, merge.
async fn tick_once(airc: &Airc, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Limit chosen large enough to surface every Review card with a
    // PR in practice. The board fetch already filters heartbeats
    // (card 79953b4d), so 256 work events back is several days of
    // realistic mutation rate.
    let board = airc.work_board(256).await?;
    let snapshot = board.snapshot();

    for card in &snapshot.cards {
        let Some(decision) = evaluate(card).await? else {
            continue;
        };
        match decision {
            MergeDecision::Merge(pr) => {
                if dry_run {
                    eprintln!(
                        "airc-merger: [DRY-RUN] would merge card={} pr=#{} ({})",
                        card.card_id, pr.number, pr.repo
                    );
                    continue;
                }
                match perform_merge(card, &pr, airc).await {
                    Ok(()) => eprintln!(
                        "airc-merger: merged card={} pr=#{} ({})",
                        card.card_id, pr.number, pr.repo
                    ),
                    Err(error) => eprintln!(
                        "airc-merger: merge failed card={} pr=#{}: {error}",
                        card.card_id, pr.number
                    ),
                }
            }
            MergeDecision::Skip(reason) => {
                eprintln!("airc-merger: skip card={} reason={reason}", card.card_id);
            }
        }
    }
    Ok(())
}

enum MergeDecision {
    Merge(airc_work::model::PullRequestRef),
    Skip(String),
}

/// Per-card eligibility check. Returns `Ok(None)` if the card isn't
/// even a candidate (not in Review, no PR linked); `Ok(Some(Merge))`
/// if everything passes; `Ok(Some(Skip(reason)))` if it's a candidate
/// that failed a gate (so we can log it instead of silently dropping).
async fn evaluate(card: &WorkCard) -> Result<Option<MergeDecision>, Box<dyn std::error::Error>> {
    use airc_work::model::CardState;
    if card.state != CardState::Review {
        return Ok(None);
    }
    let Some(pr) = card.pull_request.clone() else {
        return Ok(None);
    };

    match check_pr_gate(&pr).await {
        Ok(GateResult::Green) => Ok(Some(MergeDecision::Merge(pr))),
        Ok(GateResult::NotReady(reason)) => Ok(Some(MergeDecision::Skip(reason))),
        Err(error) => Ok(Some(MergeDecision::Skip(format!(
            "gh status query failed: {error}"
        )))),
    }
}

enum GateResult {
    Green,
    NotReady(String),
}

/// Query `gh pr view --json statusCheckRollup` and apply the merge
/// gate. First-cut policy: no FAILURE or CANCELLED in the rollup, and
/// no checks still IN_PROGRESS/PENDING. Pre-existing-red bypass is
/// deferred to a follow-up card.
async fn check_pr_gate(
    pr: &airc_work::model::PullRequestRef,
) -> Result<GateResult, Box<dyn std::error::Error>> {
    let pr_ref = format!("{}#{}", pr.repo.as_str(), pr.number);
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            pr.repo.as_str(),
            "--json",
            "statusCheckRollup,mergeable,state",
        ])
        .output()
        .await?;
    if !output.status.success() {
        return Err(format!(
            "gh pr view {pr_ref} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    let parsed: GhPrView = serde_json::from_slice(&output.stdout)?;

    if parsed.state != "OPEN" {
        return Ok(GateResult::NotReady(format!(
            "PR state is {} (not OPEN)",
            parsed.state
        )));
    }
    if parsed.mergeable == "CONFLICTING" {
        return Ok(GateResult::NotReady(
            "PR has merge conflicts; needs rebase".to_string(),
        ));
    }

    let rollup = parsed.status_check_rollup.unwrap_or_default();
    let mut failures = 0usize;
    let mut pending = 0usize;
    for c in &rollup {
        match c.conclusion.as_deref() {
            Some("SUCCESS") | Some("NEUTRAL") | Some("SKIPPED") => {}
            Some("FAILURE") | Some("CANCELLED") | Some("TIMED_OUT") => failures += 1,
            _ => {
                // No conclusion → in flight (IN_PROGRESS / QUEUED / PENDING).
                if c.status.as_deref() != Some("COMPLETED") {
                    pending += 1;
                }
            }
        }
    }
    if failures > 0 {
        return Ok(GateResult::NotReady(format!("{failures} failing check(s)")));
    }
    if pending > 0 {
        return Ok(GateResult::NotReady(format!(
            "{pending} check(s) still running"
        )));
    }
    Ok(GateResult::Green)
}

/// Actually merge. `gh pr merge --squash --delete-branch` matches the
/// convention every recent PR has used (see merged-PR survey in card
/// 28f1440c). On success, publish `MarkPullRequestMerged` so the
/// projection transitions to `Merged`.
async fn perform_merge(
    card: &WorkCard,
    pr: &airc_work::model::PullRequestRef,
    airc: &Airc,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "merge",
            &pr.number.to_string(),
            "--repo",
            pr.repo.as_str(),
            "--squash",
            "--delete-branch",
        ])
        .output()
        .await?;
    if !output.status.success() {
        return Err(format!(
            "gh pr merge #{} failed: {}",
            pr.number,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    airc.mark_pull_request_merged(MarkPullRequestMerged {
        card_id: card.card_id,
        pull_request: pr.clone(),
        merged_at_ms: now_ms,
    })
    .await?;
    Ok(())
}

#[derive(serde::Deserialize)]
struct GhPrView {
    #[serde(default)]
    state: String,
    #[serde(default, rename = "mergeable")]
    mergeable: String,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<GhCheck>>,
}

#[derive(serde::Deserialize)]
struct GhCheck {
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// Acquire a non-blocking exclusive lock at `<home>/merger.lock`.
/// Returns the held file handle (drop = release). A second launch in
/// the same scope exits with a clear error instead of racing.
fn acquire_singleton_lock(home: &Path) -> Result<std::fs::File, Box<dyn std::error::Error>> {
    use fs2::FileExt;
    std::fs::create_dir_all(home)?;
    let path = home.join("merger.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    file.try_lock_exclusive().map_err(|e| {
        format!(
            "another airc-merger is already running for {} ({}). \
             only one merger per scope at a time — kill the other or wait.",
            home.display(),
            e
        )
    })?;
    Ok(file)
}
