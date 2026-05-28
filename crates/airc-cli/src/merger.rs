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

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_lib::{Airc, MarkPullRequestMerged, WorkCard};

/// All merger output goes through here — JSON-structured stderr per
/// the log-hygiene doctrine (card 8864c548): no `println!` /
/// `eprintln!` of operational status. Stdout/stderr are reserved for
/// debug macros; substrate emits structured events the consumer
/// process (launchd, systemd, Android foreground service, log
/// aggregator) can filter and route. `StderrJsonDiagnosticSink` is
/// the same sink the daemon uses; events appear as one JSON object
/// per line on fd 2.
fn sink() -> StderrJsonDiagnosticSink {
    StderrJsonDiagnosticSink
}

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

    sink().emit(
        DiagnosticEvent::info(
            DiagnosticComponent::Merger,
            DiagnosticCode::MergerStarted,
            "continuous-merge loop started",
        )
        .with_field("home", home.display())
        .with_field("interval_ms", interval.as_millis())
        .with_field("dry_run", dry_run)
        .with_field("peer_id", airc.peer_id()),
    );

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                sink().emit(DiagnosticEvent::info(
                    DiagnosticComponent::Merger,
                    DiagnosticCode::MergerShutdown,
                    "shutdown signal received, exiting cleanly",
                ));
                return Ok(());
            }
            _ = ticker.tick() => {
                if let Err(error) = tick_once(&airc, dry_run).await {
                    // A tick failing should NOT bring down the loop —
                    // gh might be rate-limited, the daemon might be
                    // momentarily unreachable, etc. Log and continue.
                    sink().emit(
                        DiagnosticEvent::warn(
                            DiagnosticComponent::Merger,
                            DiagnosticCode::MergerTickFailed,
                            "merger tick failed",
                        )
                        .with_field("error", error),
                    );
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
    let multi_author = is_multi_author_room(&snapshot);

    for card in &snapshot.cards {
        let Some(decision) = evaluate(card, &board, multi_author).await? else {
            continue;
        };
        match decision {
            MergeDecision::Merge(pr) => {
                if dry_run {
                    sink().emit(
                        DiagnosticEvent::info(
                            DiagnosticComponent::Merger,
                            DiagnosticCode::MergerMerged,
                            "[dry-run] would merge",
                        )
                        .with_field("card_id", card.card_id)
                        .with_field("pr_number", pr.number)
                        .with_field("repo", &pr.repo)
                        .with_field("dry_run", true),
                    );
                    continue;
                }
                match perform_merge(card, &pr, airc).await {
                    Ok(()) => sink().emit(
                        DiagnosticEvent::info(
                            DiagnosticComponent::Merger,
                            DiagnosticCode::MergerMerged,
                            "merged",
                        )
                        .with_field("card_id", card.card_id)
                        .with_field("pr_number", pr.number)
                        .with_field("repo", &pr.repo),
                    ),
                    Err(error) => sink().emit(
                        DiagnosticEvent::error(
                            DiagnosticComponent::Merger,
                            DiagnosticCode::MergerMergeFailed,
                            "merge failed",
                        )
                        .with_field("card_id", card.card_id)
                        .with_field("pr_number", pr.number)
                        .with_field("error", error),
                    ),
                }
            }
            MergeDecision::Skip(reason) => {
                sink().emit(
                    DiagnosticEvent::info(
                        DiagnosticComponent::Merger,
                        DiagnosticCode::MergerSkipped,
                        "skip",
                    )
                    .with_field("card_id", card.card_id)
                    .with_field("reason", reason),
                );
            }
        }
    }
    Ok(())
}

/// Card 267d68f5: multi-author room detection. The merger's LGTM gate
/// requires a non-author peer LGTM ONLY in multi-author rooms — solo
/// scopes (one peer ever created any card) merge their own work
/// without a co-signer. Using `created_by` over `owner` is
/// deliberate: claims churn (release/reclaim), but author is fixed
/// at creation. A room with two distinct creators IS multi-author
/// even if one peer has all current claims.
fn is_multi_author_room(snapshot: &airc_work::BoardSnapshot) -> bool {
    let mut creators = std::collections::HashSet::new();
    for card in &snapshot.cards {
        creators.insert(card.created_by);
        if creators.len() > 1 {
            return true;
        }
    }
    false
}

enum MergeDecision {
    Merge(airc_work::model::PullRequestRef),
    Skip(String),
}

/// Per-card eligibility check. Returns `Ok(None)` if the card isn't
/// even a candidate (not in Review, no PR linked); `Ok(Some(Merge))`
/// if everything passes; `Ok(Some(Skip(reason)))` if it's a candidate
/// that failed a gate (so we can log it instead of silently dropping).
async fn evaluate(
    card: &WorkCard,
    projection: &airc_work::WorkBoardProjection,
    multi_author: bool,
) -> Result<Option<MergeDecision>, Box<dyn std::error::Error>> {
    use airc_work::model::CardState;
    if card.state != CardState::Review {
        return Ok(None);
    }
    let Some(pr) = card.pull_request.clone() else {
        return Ok(None);
    };

    // Card 267d68f5: peer-LGTM gate. Solo rooms (single creator
    // ever) bypass; multi-author rooms require a non-author LGTM.
    if multi_author && !projection.has_non_author_lgtm(card.card_id, &card.created_by) {
        return Ok(Some(MergeDecision::Skip(format!(
            "needs peer LGTM (multi-author room, author={})",
            card.created_by
        ))));
    }

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

/// Query `gh pr view --json statusCheckRollup,mergeable,state` and
/// apply [`evaluate_gh_view`] to the response. The IO half is here;
/// the decision half is the pure function so it can be unit-tested
/// without shelling out.
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
    Ok(evaluate_gh_view(&parsed))
}

/// Decide whether a PR is ready to merge, given the parsed `gh pr
/// view` payload. Pure — no IO, no async. First-cut policy:
///
/// - state must be `OPEN` (not already MERGED/CLOSED)
/// - mergeable must not be `CONFLICTING` (no rebase needed)
/// - no `FAILURE` / `CANCELLED` / `TIMED_OUT` conclusions
/// - no still-running checks (status != COMPLETED with no conclusion)
///
/// The strictly-less-red-than-base doctrine refinement (#1033) is a
/// separate, more lenient gate carded as a follow-up.
fn evaluate_gh_view(view: &GhPrView) -> GateResult {
    if view.state != "OPEN" {
        return GateResult::NotReady(format!("PR state is {} (not OPEN)", view.state));
    }
    if view.mergeable == "CONFLICTING" {
        return GateResult::NotReady("PR has merge conflicts; needs rebase".to_string());
    }
    let rollup = view.status_check_rollup.as_deref().unwrap_or(&[]);
    let mut failures = 0usize;
    let mut pending = 0usize;
    for c in rollup {
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
        return GateResult::NotReady(format!("{failures} failing check(s)"));
    }
    if pending > 0 {
        return GateResult::NotReady(format!("{pending} check(s) still running"));
    }
    GateResult::Green
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

#[cfg(test)]
mod tests {
    //! Pure-function tests for the merge gate. The IO and orchestration
    //! halves (gh shelling, work_board fetch, the loop) are integration
    //! territory — exercised end-to-end when the merger runs against
    //! real PRs. The decision matrix is what changes most often (every
    //! follow-up to f16650cd: LGTM gate, less-red bypass, catchup); it
    //! is the right surface to lock down with unit tests.
    use super::*;
    use serde_json::json;

    fn parse(payload: serde_json::Value) -> GhPrView {
        serde_json::from_value(payload).expect("test fixture must parse")
    }

    #[test]
    fn merges_when_state_open_no_conflicts_all_checks_green() {
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "status": "COMPLETED"},
                {"conclusion": "NEUTRAL", "status": "COMPLETED"},
                {"conclusion": "SKIPPED", "status": "COMPLETED"},
            ]
        }));
        assert!(matches!(evaluate_gh_view(&view), GateResult::Green));
    }

    #[test]
    fn refuses_when_pr_is_closed() {
        let view = parse(json!({"state": "CLOSED", "mergeable": "MERGEABLE"}));
        let result = evaluate_gh_view(&view);
        let GateResult::NotReady(reason) = result else {
            panic!("expected NotReady, got Green");
        };
        assert!(reason.contains("CLOSED"), "reason should name the state");
    }

    #[test]
    fn refuses_when_pr_is_already_merged() {
        let view = parse(json!({"state": "MERGED", "mergeable": "MERGEABLE"}));
        assert!(matches!(evaluate_gh_view(&view), GateResult::NotReady(_)));
    }

    #[test]
    fn refuses_when_conflicting() {
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "CONFLICTING",
            "statusCheckRollup": [{"conclusion": "SUCCESS", "status": "COMPLETED"}]
        }));
        let GateResult::NotReady(reason) = evaluate_gh_view(&view) else {
            panic!("expected NotReady");
        };
        assert!(
            reason.contains("conflict") || reason.contains("rebase"),
            "reason should name conflicts"
        );
    }

    #[test]
    fn refuses_when_any_check_failed() {
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "status": "COMPLETED"},
                {"conclusion": "FAILURE", "status": "COMPLETED"},
                {"conclusion": "SUCCESS", "status": "COMPLETED"},
            ]
        }));
        let GateResult::NotReady(reason) = evaluate_gh_view(&view) else {
            panic!("expected NotReady");
        };
        assert!(reason.contains("failing"), "reason should mention failures");
    }

    #[test]
    fn refuses_when_a_check_was_cancelled() {
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "CANCELLED", "status": "COMPLETED"},
            ]
        }));
        assert!(matches!(evaluate_gh_view(&view), GateResult::NotReady(_)));
    }

    #[test]
    fn refuses_when_a_check_timed_out() {
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "TIMED_OUT", "status": "COMPLETED"},
            ]
        }));
        assert!(matches!(evaluate_gh_view(&view), GateResult::NotReady(_)));
    }

    #[test]
    fn refuses_when_a_check_is_still_running() {
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "status": "COMPLETED"},
                {"conclusion": null, "status": "IN_PROGRESS"},
            ]
        }));
        let GateResult::NotReady(reason) = evaluate_gh_view(&view) else {
            panic!("expected NotReady");
        };
        assert!(
            reason.contains("still running"),
            "reason should indicate pending checks"
        );
    }

    #[test]
    fn merges_when_rollup_is_empty() {
        // No checks configured at all → mergeable. (Repo with no CI is
        // a valid state, e.g. docs-only repos.) The gate is "no
        // failure OR pending"; zero checks satisfies both vacuously.
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [],
        }));
        assert!(matches!(evaluate_gh_view(&view), GateResult::Green));
    }

    #[test]
    fn merges_when_rollup_field_absent() {
        // Missing field → default empty Vec → vacuously green, same
        // as the explicit-empty case above. The serde default keeps
        // the gate robust to gh CLI version drift.
        let view = parse(json!({"state": "OPEN", "mergeable": "MERGEABLE"}));
        assert!(matches!(evaluate_gh_view(&view), GateResult::Green));
    }

    #[test]
    fn singleton_lock_refuses_second_holder() {
        // Acquire on a fresh tmpdir, then try a second acquire — must
        // fail with the "already running" error rather than blocking
        // or succeeding (which would race two mergers).
        let tmp = std::env::temp_dir().join(format!(
            "airc-merger-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let _first = acquire_singleton_lock(&tmp).expect("first acquire");
        let second = acquire_singleton_lock(&tmp);
        assert!(second.is_err(), "second acquire must fail");
        let err = second.unwrap_err().to_string();
        assert!(
            err.contains("already running"),
            "error should name the conflict: {err}"
        );

        drop(_first);
        let _third = acquire_singleton_lock(&tmp).expect("after drop, third acquire succeeds");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
