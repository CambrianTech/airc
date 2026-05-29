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

    // Cards dec35ec7 + a094aa81: one ShellGhClient per merger session,
    // passed downstream as `&dyn GhClient` so an alternative impl
    // (mock, HTTP-direct) can substitute at the boundary without
    // touching the merger's gate logic.
    let gh = crate::gh_client::ShellGhClient::new();

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
                if let Err(error) = tick_once(&gh, &airc, dry_run).await {
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
async fn tick_once(
    gh: &dyn crate::gh_client::GhClient,
    airc: &Airc,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Limit chosen large enough to surface every Review card with a
    // PR in practice. The board fetch already filters heartbeats
    // (card 79953b4d), so 256 work events back is several days of
    // realistic mutation rate.
    let board = airc.work_board(256).await?;
    let snapshot = board.snapshot();

    // Card d5b7b07d: fetch the baseline (rust-rewrite HEAD's failing
    // check names) ONCE per tick — every per-card gate consults the
    // same snapshot. The set is small (handful of check names) and
    // the query is one REST call; cheap relative to per-PR pr_view.
    let baseline_failures = fetch_baseline_failures(gh).await;
    if !baseline_failures.is_empty() {
        eprintln!(
            "airc-merger: baseline has {} failing check(s) on rust-rewrite — \
             those won't block per-PR gates this tick",
            baseline_failures.len()
        );
    }

    for card in &snapshot.cards {
        let Some(decision) = evaluate(gh, card, &baseline_failures).await? else {
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
                match perform_merge(gh, card, &pr, airc).await {
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
async fn evaluate(
    gh: &dyn crate::gh_client::GhClient,
    card: &WorkCard,
    baseline_failures: &std::collections::HashSet<String>,
) -> Result<Option<MergeDecision>, Box<dyn std::error::Error>> {
    use airc_work::model::CardState;
    if card.state != CardState::Review {
        return Ok(None);
    }
    let Some(pr) = card.pull_request.clone() else {
        return Ok(None);
    };

    match check_pr_gate(gh, &pr, baseline_failures).await {
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
/// apply [`evaluate_gh_view`] to the response.
///
/// Card dec35ec7: the IO half routes through the typed
/// [`crate::gh_client::GhClient`] instead of a raw
/// `tokio::process::Command::new("gh")`. Errors come back as
/// typed [`crate::gh_client::GhError`] variants (rate-limited,
/// auth-required, …) so callers can pattern-match on them; the
/// decision half (`evaluate_gh_view`) remains pure for unit tests.
async fn check_pr_gate(
    gh: &dyn crate::gh_client::GhClient,
    pr: &airc_work::model::PullRequestRef,
    baseline_failures: &std::collections::HashSet<String>,
) -> Result<GateResult, crate::gh_client::GhError> {
    let view = gh
        .pr_view(crate::gh_client::PrViewArgs {
            repo: pr.repo.as_str().to_string(),
            number: pr.number,
            cwd: None,
        })
        .await?;
    Ok(evaluate_gh_view(&view, baseline_failures))
}

/// Fetch the rust-rewrite HEAD's check-run rollup and return the SET
/// of names that are currently FAILURE on base. The merger calls this
/// once per tick; each per-card gate consults the same snapshot. On
/// error (rate-limit, network), returns empty set — the gate
/// degrades to "no allowance" rather than over-trusting.
async fn fetch_baseline_failures(
    gh: &dyn crate::gh_client::GhClient,
) -> std::collections::HashSet<String> {
    let base_branch = crate::work_commands_gh::pr_create_base_branch();
    let runs = match gh
        .branch_check_rollup(crate::gh_client::BranchCheckRollupArgs {
            repo: "CambrianTech/airc".to_string(),
            branch: base_branch.to_string(),
        })
        .await
    {
        Ok(runs) => runs,
        Err(error) => {
            eprintln!(
                "airc-merger: baseline-failures lookup failed for {base_branch}: {error} \
                 (degrading to no-allowance gate)"
            );
            return std::collections::HashSet::new();
        }
    };
    baseline_failing_names(&runs)
}

/// Pure projection: rollup → set of check NAMES whose conclusion is
/// failure-shaped (case-insensitive). Kept separate from the IO half
/// so the strictly-less-red logic is unit-testable without a live
/// `gh`.
pub(crate) fn baseline_failing_names(
    runs: &[crate::gh_client::GhCheck],
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for c in runs {
        let conc_upper = c
            .conclusion
            .as_deref()
            .map(|s| s.to_ascii_uppercase())
            .unwrap_or_default();
        if matches!(conc_upper.as_str(), "FAILURE" | "CANCELLED" | "TIMED_OUT") {
            if let Some(name) = c.name.as_deref() {
                set.insert(name.to_string());
            }
        }
    }
    set
}

/// Decide whether a PR is ready to merge, given the parsed `gh pr
/// view` payload. Pure — no IO, no async. First-cut policy:
///
/// - state must be `OPEN` (not already MERGED/CLOSED)
/// - mergeable must not be `CONFLICTING` (no rebase needed)
/// - no `FAILURE` / `CANCELLED` / `TIMED_OUT` conclusions
/// - no still-running checks (status != COMPLETED with no conclusion)
///
/// Card d5b7b07d adds the strictly-less-red-than-base refinement:
/// a FAILURE whose check name is ALSO failing on base is treated as
/// effectively neutral (the PR didn't cause it). Pass an empty set
/// to disable the bypass — that's the f16650cd-original strict gate.
fn evaluate_gh_view(
    view: &crate::gh_client::PrView,
    baseline_failures: &std::collections::HashSet<String>,
) -> GateResult {
    if view.state != "OPEN" {
        return GateResult::NotReady(format!("PR state is {} (not OPEN)", view.state));
    }
    if view.mergeable == "CONFLICTING" {
        return GateResult::NotReady("PR has merge conflicts; needs rebase".to_string());
    }
    let rollup = view.status_check_rollup.as_deref().unwrap_or(&[]);
    let mut new_failures = 0usize;
    let mut inherited_failures = 0usize;
    let mut pending = 0usize;
    for c in rollup {
        match c.conclusion.as_deref() {
            Some("SUCCESS") | Some("NEUTRAL") | Some("SKIPPED") => {}
            Some("FAILURE") | Some("CANCELLED") | Some("TIMED_OUT") => {
                // Strictly-less-red: if the PR-side check name is
                // also failing on base, the PR isn't responsible.
                // Counts as inherited (doesn't block) so the log line
                // can surface that we noticed.
                let is_inherited = c
                    .name
                    .as_deref()
                    .map(|n| baseline_failures.contains(n))
                    .unwrap_or(false);
                if is_inherited {
                    inherited_failures += 1;
                } else {
                    new_failures += 1;
                }
            }
            _ => {
                // No conclusion → in flight (IN_PROGRESS / QUEUED / PENDING).
                if c.status.as_deref() != Some("COMPLETED") {
                    pending += 1;
                }
            }
        }
    }
    if new_failures > 0 {
        let inherited_note = if inherited_failures > 0 {
            format!(" ({inherited_failures} inherited from base, ignored)")
        } else {
            String::new()
        };
        return GateResult::NotReady(format!("{new_failures} failing check(s){inherited_note}"));
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
/// Card dec35ec7: typed `gh pr merge` via `GhClient`. `GhError`
/// surfaces conflicts as `PrNotMergeable` so an upcoming
/// less-red-than-base bypass can match the case and skip rather
/// than retry indefinitely. `MarkPullRequestMerged` still fires
/// after the merge succeeds — the projection contract is unchanged.
async fn perform_merge(
    gh: &dyn crate::gh_client::GhClient,
    card: &WorkCard,
    pr: &airc_work::model::PullRequestRef,
    airc: &Airc,
) -> Result<(), Box<dyn std::error::Error>> {
    gh.pr_merge(crate::gh_client::PrMergeArgs {
        repo: pr.repo.as_str().to_string(),
        number: pr.number,
    })
    .await?;

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

    fn parse(payload: serde_json::Value) -> crate::gh_client::PrView {
        // Card dec35ec7: tests now exercise the typed PrView from
        // GhClient, not a merger-local duplicate struct.
        serde_json::from_value(payload).expect("test fixture must parse")
    }

    /// Empty baseline = no inherited failures known. Old strict-gate
    /// behavior. Used by all the pre-d5b7b07d tests which pre-date
    /// the strictly-less-red bypass.
    fn empty_baseline() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
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
        assert!(matches!(
            evaluate_gh_view(&view, &empty_baseline()),
            GateResult::Green
        ));
    }

    #[test]
    fn refuses_when_pr_is_closed() {
        let view = parse(json!({"state": "CLOSED", "mergeable": "MERGEABLE"}));
        let result = evaluate_gh_view(&view, &empty_baseline());
        let GateResult::NotReady(reason) = result else {
            panic!("expected NotReady, got Green");
        };
        assert!(reason.contains("CLOSED"), "reason should name the state");
    }

    #[test]
    fn refuses_when_pr_is_already_merged() {
        let view = parse(json!({"state": "MERGED", "mergeable": "MERGEABLE"}));
        assert!(matches!(
            evaluate_gh_view(&view, &empty_baseline()),
            GateResult::NotReady(_)
        ));
    }

    #[test]
    fn refuses_when_conflicting() {
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "CONFLICTING",
            "statusCheckRollup": [{"conclusion": "SUCCESS", "status": "COMPLETED"}]
        }));
        let GateResult::NotReady(reason) = evaluate_gh_view(&view, &empty_baseline()) else {
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
        let GateResult::NotReady(reason) = evaluate_gh_view(&view, &empty_baseline()) else {
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
        assert!(matches!(
            evaluate_gh_view(&view, &empty_baseline()),
            GateResult::NotReady(_)
        ));
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
        assert!(matches!(
            evaluate_gh_view(&view, &empty_baseline()),
            GateResult::NotReady(_)
        ));
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
        let GateResult::NotReady(reason) = evaluate_gh_view(&view, &empty_baseline()) else {
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
        assert!(matches!(
            evaluate_gh_view(&view, &empty_baseline()),
            GateResult::Green
        ));
    }

    #[test]
    fn merges_when_rollup_field_absent() {
        // Missing field → default empty Vec → vacuously green, same
        // as the explicit-empty case above. The serde default keeps
        // the gate robust to gh CLI version drift.
        let view = parse(json!({"state": "OPEN", "mergeable": "MERGEABLE"}));
        assert!(matches!(
            evaluate_gh_view(&view, &empty_baseline()),
            GateResult::Green
        ));
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

    // ------------------------------------------------------------------
    // Card d5b7b07d — strictly-less-red-than-base
    // ------------------------------------------------------------------

    fn baseline_with(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn evaluate_ignores_failure_whose_name_is_in_baseline() {
        // Card d5b7b07d: the PR has one FAILURE, but that check name
        // is already failing on base. PR didn't cause it; gate must
        // pass.
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "SUCCESS", "status": "COMPLETED", "name": "cargo test (macos-latest)"},
                {"conclusion": "FAILURE", "status": "COMPLETED", "name": "shell syntax + rust cutover guards"},
            ]
        }));
        let baseline = baseline_with(&["shell syntax + rust cutover guards"]);
        assert!(matches!(
            evaluate_gh_view(&view, &baseline),
            GateResult::Green
        ));
    }

    #[test]
    fn evaluate_still_refuses_when_pr_introduces_new_failure() {
        // Card d5b7b07d: even with one inherited failure, a NEW
        // PR-side failure (different name) blocks. The bypass is
        // narrow — it only forgives names that also fail on base.
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "FAILURE", "status": "COMPLETED", "name": "shell syntax + rust cutover guards"},
                {"conclusion": "FAILURE", "status": "COMPLETED", "name": "cargo test (ubuntu-latest)"},
            ]
        }));
        let baseline = baseline_with(&["shell syntax + rust cutover guards"]);
        let GateResult::NotReady(reason) = evaluate_gh_view(&view, &baseline) else {
            panic!("expected NotReady");
        };
        assert!(
            reason.contains("1 failing") && reason.contains("inherited"),
            "reason should distinguish new from inherited: {reason}"
        );
    }

    #[test]
    fn evaluate_with_empty_baseline_matches_old_strict_behavior() {
        // Regression: when no baseline known (lookup failed) the gate
        // falls back to the f16650cd-original strict behavior — no
        // free passes.
        let view = parse(json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"conclusion": "FAILURE", "status": "COMPLETED", "name": "shell syntax + rust cutover guards"},
            ]
        }));
        let GateResult::NotReady(reason) = evaluate_gh_view(&view, &empty_baseline()) else {
            panic!("expected NotReady");
        };
        assert!(
            reason.contains("1 failing"),
            "reason should be unchanged: {reason}"
        );
    }

    #[test]
    fn baseline_failing_names_picks_up_failure_shaped_conclusions() {
        // Card d5b7b07d: the REST endpoint uses lowercase ("failure")
        // while gh pr view uses uppercase. baseline_failing_names
        // must accept both — a case mismatch would silently drop the
        // base failure and re-block every PR. Also: CANCELLED /
        // TIMED_OUT count as failure-shaped because they're
        // failure-shaped from the merger's perspective (something
        // went wrong, not "the PR passed").
        let runs = vec![
            crate::gh_client::GhCheck {
                conclusion: Some("failure".to_string()),
                status: Some("completed".to_string()),
                name: Some("shell syntax + rust cutover guards".to_string()),
            },
            crate::gh_client::GhCheck {
                conclusion: Some("FAILURE".to_string()),
                status: Some("COMPLETED".to_string()),
                name: Some("clean-install-linux".to_string()),
            },
            crate::gh_client::GhCheck {
                conclusion: Some("cancelled".to_string()),
                status: Some("completed".to_string()),
                name: Some("clean-install-macos".to_string()),
            },
            crate::gh_client::GhCheck {
                conclusion: Some("success".to_string()),
                status: Some("completed".to_string()),
                name: Some("cargo fmt --check".to_string()),
            },
        ];
        let set = baseline_failing_names(&runs);
        assert_eq!(set.len(), 3);
        assert!(set.contains("shell syntax + rust cutover guards"));
        assert!(set.contains("clean-install-linux"));
        assert!(set.contains("clean-install-macos"));
        assert!(!set.contains("cargo fmt --check"));
    }

    #[test]
    fn baseline_failing_names_returns_empty_when_base_is_green() {
        // Regression: when base is all-green, the set is empty and
        // the merger gates as old-strict — exactly what we want.
        let runs = vec![
            crate::gh_client::GhCheck {
                conclusion: Some("success".to_string()),
                status: Some("completed".to_string()),
                name: Some("cargo fmt --check".to_string()),
            },
            crate::gh_client::GhCheck {
                conclusion: Some("success".to_string()),
                status: Some("completed".to_string()),
                name: Some("cargo test (ubuntu-latest)".to_string()),
            },
        ];
        assert!(baseline_failing_names(&runs).is_empty());
    }

    #[test]
    fn parse_check_runs_handles_rest_envelope() {
        // The REST `/check-runs` endpoint wraps results in a
        // {total_count, check_runs: [...]} envelope. Pin that we
        // project to just the run list — anything else (like
        // returning the whole envelope) and the merger can't
        // typecheck against GhCheck.
        let json = serde_json::json!({
            "total_count": 2,
            "check_runs": [
                {"name": "cargo fmt --check", "status": "completed", "conclusion": "success"},
                {"name": "cargo test (windows-latest)", "status": "in_progress", "conclusion": null},
            ]
        });
        let runs = airc_lib::gh_client::parse_check_runs(json.to_string().as_bytes())
            .expect("envelope parses");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].name.as_deref(), Some("cargo fmt --check"));
        assert_eq!(runs[1].conclusion.as_deref(), None);
        assert_eq!(runs[1].status.as_deref(), Some("in_progress"));
    }
}
