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

/// Pending-timeout disabled + fixed clock — strict pre-7ed1ac4f
/// behavior. Used by all pre-7ed1ac4f tests so they keep
/// asserting the same shape; the new pending-timeout tests
/// build their own GatePolicy with a known clock.
fn empty_policy() -> GatePolicy {
    GatePolicy {
        pending_timeout_ms: 0,
        now_ms: 0,
    }
}

fn linked_pr(repo: &str, number: u64) -> airc_work::model::PullRequestRef {
    airc_work::model::PullRequestRef {
        repo: airc_work::ids::RepoId::new(repo).unwrap(),
        number,
        head: airc_work::model::BranchName::new("topic").unwrap(),
        // Deliberately stale: live PR metadata must select the baseline.
        base: airc_work::model::BranchName::new("old-target").unwrap(),
    }
}

fn failing_pr_view(branch: &str, revision: &str) -> crate::gh_client::PrView {
    parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "baseRefName": branch,
        "baseRefOid": revision,
        "statusCheckRollup": [
            {"name": "cargo test", "status": "COMPLETED", "conclusion": "FAILURE"}
        ]
    }))
}

// What this catches (045083e8): a shared check name in AIRC must not
// excuse a new Continuum failure. The actual PR target also wins over
// a stale card link; moving either the ref or its SHA starts a new baseline.
#[tokio::test]
async fn merge_gate_isolates_baselines_by_live_repo_branch_and_revision() {
    use airc_lib::gh::client::mock::MockGhClient;

    let gh = MockGhClient::new();
    let first = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let next = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let mut baselines = BaselineCache::default();
    for (number, repo, branch, revision, should_merge) in [
        (1, "CambrianTech/airc", "canary", first, true),
        (2, "CambrianTech/continuum", "canary", first, false),
        (3, "CambrianTech/airc", "canary", first, true),
        (4, "CambrianTech/airc", "release/topic", first, false),
        (5, "CambrianTech/airc", "canary", next, false),
    ] {
        let view = failing_pr_view(branch, revision);
        if number == 1 {
            gh.queue_branch_check_rollup(Ok(view.status_check_rollup.clone().unwrap()));
        } else if number != 3 {
            gh.queue_branch_check_rollup(Ok(Vec::new()));
        }
        gh.queue_pr_view(Ok(view));
        let result = check_pr_gate(
            &gh,
            &linked_pr(repo, number),
            &mut baselines,
            empty_policy(),
        )
        .await
        .unwrap();
        assert_eq!(
            matches!(result, GateResult::Green),
            should_merge,
            "PR {number} {repo}:{branch}@{revision}: {result:?}"
        );
    }
    let calls = gh.received_branch_check_rollup_calls();
    let targets: Vec<_> = calls
        .iter()
        .map(|call| (call.repo.as_str(), call.branch.as_str()))
        .collect();
    assert_eq!(
        targets,
        vec![
            ("CambrianTech/airc", first),
            ("CambrianTech/continuum", first),
            ("CambrianTech/airc", first),
            ("CambrianTech/airc", next),
        ],
        "only matching repo/ref/SHA shares a lookup; query immutable SHA, never card.base"
    );
    assert_eq!(
        gh.pr_view_call_count(),
        5,
        "each PR gets a live status read"
    );
}

// What this catches (045083e8): missing metadata, ambiguous refs, and
// lookup errors grant no exemption. An independently green PR remains
// green without needing a baseline request or inventing a target.
#[tokio::test]
async fn merge_gate_missing_or_failed_baseline_preserves_strict_outcome() {
    use airc_lib::gh::client::{mock::MockGhClient, GhError};

    let gh = MockGhClient::new();
    let revision = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let pr = linked_pr("CambrianTech/continuum", 42);
    let mut baselines = BaselineCache::default();
    for (branch, oid) in [
        (None, Some(revision)),
        (Some("canary"), None),
        (Some(""), Some(revision)),
        (Some("canary"), Some("abcd")),
        (Some("canary"), Some("refs/heads/canary")),
    ] {
        let mut view = failing_pr_view("canary", revision);
        view.base_ref_name = branch.map(str::to_string);
        view.base_ref_oid = oid.map(str::to_string);
        gh.queue_pr_view(Ok(view));
        assert!(matches!(
            check_pr_gate(&gh, &pr, &mut baselines, empty_policy())
                .await
                .unwrap(),
            GateResult::NotReady(_)
        ));
    }
    assert_eq!(gh.branch_check_rollup_call_count(), 0);

    gh.queue_branch_check_rollup(Err(GhError::RateLimited {
        stderr: "fixture rate limit".into(),
    }));
    for _ in 0..2 {
        gh.queue_pr_view(Ok(failing_pr_view("canary", revision)));
        assert!(matches!(
            check_pr_gate(&gh, &pr, &mut baselines, empty_policy())
                .await
                .unwrap(),
            GateResult::NotReady(_)
        ));
    }
    assert_eq!(
        gh.branch_check_rollup_call_count(),
        1,
        "failed lookup is cached for this tick"
    );

    gh.queue_pr_view(Ok(parse(json!({
        "state": "OPEN", "mergeable": "MERGEABLE",
        "statusCheckRollup": [{"name": "cargo test", "status": "COMPLETED", "conclusion": "SUCCESS"}]
    }))));
    assert!(matches!(
        check_pr_gate(&gh, &pr, &mut baselines, empty_policy())
            .await
            .unwrap(),
        GateResult::Green
    ));
    assert_eq!(
        gh.branch_check_rollup_call_count(),
        1,
        "green needs no allowance"
    );

    // A later tick/manual command retries; a transient error is not a
    // permanent exemption or a process-global negative cache.
    gh.queue_pr_view(Ok(failing_pr_view("canary", revision)));
    gh.queue_branch_check_rollup(Ok(failing_pr_view("canary", revision)
        .status_check_rollup
        .unwrap()));
    assert!(matches!(
        check_pr_gate(&gh, &pr, &mut BaselineCache::default(), empty_policy())
            .await
            .unwrap(),
        GateResult::Green
    ));
    assert_eq!(gh.branch_check_rollup_call_count(), 2);
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
        evaluate_gh_view(&view, &empty_baseline(), empty_policy()),
        GateResult::Green
    ));
}

#[test]
fn refuses_when_pr_is_closed() {
    let view = parse(json!({"state": "CLOSED", "mergeable": "MERGEABLE"}));
    let result = evaluate_gh_view(&view, &empty_baseline(), empty_policy());
    let GateResult::NotReady(reason) = result else {
        panic!("expected NotReady, got Green");
    };
    assert!(reason.contains("CLOSED"), "reason should name the state");
}

#[test]
fn reconciles_when_pr_is_already_merged() {
    // Pre-acd72c81 behavior: MERGED → NotReady("PR state is
    // MERGED (not OPEN)"). That caused the merger to skip
    // already-merged PRs forever, leaving the kanban out of
    // sync with GitHub. The acd72c81 follow-up routes MERGED
    // into AlreadyMerged so the merger emits
    // `PullRequestMerged` and transitions the card.
    let view = parse(
        json!({"state": "MERGED", "mergeable": "MERGEABLE", "mergedAt": "2026-06-01T07:04:07Z"}),
    );
    match evaluate_gh_view(&view, &empty_baseline(), empty_policy()) {
        GateResult::AlreadyMerged { merged_at_ms } => {
            assert_eq!(merged_at_ms, 1780297447000);
        }
        other => panic!("expected AlreadyMerged, got {other:?}"),
    }
}

#[test]
fn refuses_when_conflicting() {
    let view = parse(json!({
        "state": "OPEN",
        "mergeable": "CONFLICTING",
        "statusCheckRollup": [{"conclusion": "SUCCESS", "status": "COMPLETED"}]
    }));
    let GateResult::NotReady(reason) =
        evaluate_gh_view(&view, &empty_baseline(), empty_policy())
    else {
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
    let GateResult::NotReady(reason) =
        evaluate_gh_view(&view, &empty_baseline(), empty_policy())
    else {
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
        evaluate_gh_view(&view, &empty_baseline(), empty_policy()),
        GateResult::NotReady(_)
    ));
}

// regression for airc #1539: a force-push's cancelled runs stood beside
// their green re-runs and the gate refused "13 failing check(s)".
// what this catches: a superseded run no longer votes, a re-run that is
// still red still blocks, and a run without a timestamp is never dropped.
#[test]
fn only_the_latest_run_of_each_check_votes() {
    let superseded = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"name": "cargo test", "conclusion": "CANCELLED", "status": "COMPLETED",
             "startedAt": "2026-10-05T15:59:40Z"},
            {"name": "cargo test", "conclusion": "SUCCESS", "status": "COMPLETED",
             "startedAt": "2026-10-05T16:05:00Z"},
        ]
    }));
    assert!(matches!(
        evaluate_gh_view(&superseded, &empty_baseline(), empty_policy()),
        GateResult::Green
    ));

    let rerun_still_red = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"name": "cargo test", "conclusion": "SUCCESS", "status": "COMPLETED",
             "startedAt": "2026-10-05T15:59:40Z"},
            {"name": "cargo test", "conclusion": "FAILURE", "status": "COMPLETED",
             "startedAt": "2026-10-05T16:05:00Z"},
        ]
    }));
    assert!(matches!(
        evaluate_gh_view(&rerun_still_red, &empty_baseline(), empty_policy()),
        GateResult::NotReady(_)
    ));

    let undated = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"name": "cargo test", "conclusion": "CANCELLED", "status": "COMPLETED"},
            {"name": "cargo test", "conclusion": "SUCCESS", "status": "COMPLETED",
             "startedAt": "2026-10-05T16:05:00Z"},
        ]
    }));
    assert!(matches!(
        evaluate_gh_view(&undated, &empty_baseline(), empty_policy()),
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
        evaluate_gh_view(&view, &empty_baseline(), empty_policy()),
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
    let GateResult::NotReady(reason) =
        evaluate_gh_view(&view, &empty_baseline(), empty_policy())
    else {
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
        evaluate_gh_view(&view, &empty_baseline(), empty_policy()),
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
        evaluate_gh_view(&view, &empty_baseline(), empty_policy()),
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
    // The `flock(LOCK_EX)` held by `_first` is released when its file descriptor
    // closes on drop. That release is synchronous on macOS, but on some Linux
    // kernels/filesystems (seen on the ubuntu CI runner) the handoff to a fresh
    // `open()` + `flock()` can lag a few ms — a real property of advisory locks,
    // not a bug in the code under test. Production never rapid-re-acquires (the
    // merger holds the lock for its whole lifetime), so a short bounded wait here
    // reflects real lock-handoff timing and makes the test deterministic instead
    // of platform-flaky. Fails loud if the lock is genuinely never released.
    let mut third = None;
    for _ in 0..100 {
        match acquire_singleton_lock(&tmp) {
            Ok(f) => {
                third = Some(f);
                break;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    let _third =
        third.expect("after drop, third acquire must succeed once the flock releases (<=2s)");

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
        evaluate_gh_view(&view, &baseline, empty_policy()),
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
    let GateResult::NotReady(reason) = evaluate_gh_view(&view, &baseline, empty_policy())
    else {
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
    let GateResult::NotReady(reason) =
        evaluate_gh_view(&view, &empty_baseline(), empty_policy())
    else {
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
            started_at: None,
        },
        crate::gh_client::GhCheck {
            conclusion: Some("FAILURE".to_string()),
            status: Some("COMPLETED".to_string()),
            name: Some("clean-install-linux".to_string()),
            started_at: None,
        },
        crate::gh_client::GhCheck {
            conclusion: Some("cancelled".to_string()),
            status: Some("completed".to_string()),
            name: Some("clean-install-macos".to_string()),
            started_at: None,
        },
        crate::gh_client::GhCheck {
            conclusion: Some("success".to_string()),
            status: Some("completed".to_string()),
            name: Some("cargo fmt --check".to_string()),
            started_at: None,
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
            started_at: None,
        },
        crate::gh_client::GhCheck {
            conclusion: Some("success".to_string()),
            status: Some("completed".to_string()),
            name: Some("cargo test (ubuntu-latest)".to_string()),
            started_at: None,
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
            {"id": 1, "name": "cargo fmt --check", "status": "completed", "conclusion": "success"},
            {"id": 2, "name": "cargo test (windows-latest)", "status": "in_progress", "conclusion": null},
        ]
    });
    let runs = airc_lib::gh::client::parse_check_runs(json.to_string().as_bytes())
        .expect("envelope parses");
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].name.as_deref(), Some("cargo fmt --check"));
    assert_eq!(runs[1].conclusion.as_deref(), None);
    // Card c03bb62f: GhCheck is GraphQL-cased at the parser now — REST's
    // `in_progress` surfaces as `IN_PROGRESS`, the vocabulary the gate
    // (`Some("COMPLETED")` / `Some("SUCCESS")`) actually speaks.
    assert_eq!(runs[1].status.as_deref(), Some("IN_PROGRESS"));
}

// ------------------------------------------------------------------
// Card a399b342 — `airc work merge` (manual gate). The IO path
// (gh.pr_view + gh.pr_merge + MarkPullRequestMerged emit) is
// exercised by integration tests against real PRs in the merger
// dry-run mode. Here we pin the pub(crate) gate surface stays
// observable to work_commands::run_merge — a regression that
// narrows GateResult / check_pr_gate back to private would break
// the engineering-discipline gate without surfacing in clippy
// (private items are still implemented; just unreachable from
// work_commands.rs).
// ------------------------------------------------------------------

#[test]
fn gate_result_is_reachable_for_run_merge_callers() {
    // Compile-time assertion: GateResult variants are
    // pattern-matchable from a function that is NOT in the merger
    // module. If a future change re-privatises these, this test
    // fails to compile rather than the cli `work merge` path
    // silently losing its gate.
    fn classify_for_external_caller(g: GateResult) -> &'static str {
        match g {
            GateResult::Green => "green",
            GateResult::NotReady(_) => "not_ready",
            GateResult::AlreadyMerged { .. } => "already_merged",
        }
    }
    assert_eq!(classify_for_external_caller(GateResult::Green), "green");
    assert_eq!(
        classify_for_external_caller(GateResult::NotReady("test".into())),
        "not_ready"
    );
    assert_eq!(
        classify_for_external_caller(GateResult::AlreadyMerged { merged_at_ms: 0 }),
        "already_merged"
    );
}

#[test]
fn evaluate_gh_view_routes_merged_state_to_already_merged() {
    // Card acd72c81 follow-up: a PR whose state is MERGED
    // returns AlreadyMerged so the merger reconciles instead of
    // skipping. The parsed mergedAt becomes the canonical
    // merged_at_ms; absent mergedAt falls back to wall-clock-now
    // (not asserted here because that path uses SystemTime).
    let view = crate::gh_client::PrView {
        state: "MERGED".to_string(),
        mergeable: "".to_string(),
        status_check_rollup: None,
        merged_at: Some("2026-06-01T07:04:07Z".to_string()),
        base_ref_name: None,
        base_ref_oid: None,
    };
    let baseline = std::collections::HashSet::new();
    let policy = GatePolicy::default_for_merger(0);
    match evaluate_gh_view(&view, &baseline, policy) {
        GateResult::AlreadyMerged { merged_at_ms } => {
            // 2026-06-01T07:04:07Z = 1780297447000 ms
            assert_eq!(merged_at_ms, 1780297447000);
        }
        other => panic!("expected AlreadyMerged, got {other:?}"),
    }
}

// what this catches: the production ReqwestGhClient reads REST, where a
// merged PR is `state: closed` + `merged_at: <ts>`; only GraphQL says
// MERGED. A string-only gate skipped every REST-viewed merged PR and the
// card sat at Review forever (card c03bb62f, 2026-09-04 receipts:
// continuum #3692/#3694/#3695). Merged is the `merged_at` FACT.
#[test]
fn evaluate_gh_view_reconciles_rest_shaped_closed_plus_merged_at() {
    let view = crate::gh_client::PrView {
        state: "CLOSED".to_string(),
        mergeable: "".to_string(),
        status_check_rollup: None,
        merged_at: Some("2026-09-04T16:46:08Z".to_string()),
        base_ref_name: None,
        base_ref_oid: None,
    };
    match evaluate_gh_view(&view, &empty_baseline(), empty_policy()) {
        GateResult::AlreadyMerged { merged_at_ms } => {
            assert_eq!(
                merged_at_ms,
                parse_iso8601_to_ms("2026-09-04T16:46:08Z").unwrap()
            );
        }
        other => panic!("expected AlreadyMerged, got {other:?}"),
    }
}

// what this catches: a PR closed WITHOUT merging must never be
// reconciled as merged — `merged_at` absent means the fact is absent.
#[test]
fn evaluate_gh_view_does_not_reconcile_closed_unmerged() {
    let view = crate::gh_client::PrView {
        state: "CLOSED".to_string(),
        mergeable: "".to_string(),
        status_check_rollup: None,
        merged_at: None,
        base_ref_name: None,
        base_ref_oid: None,
    };
    match evaluate_gh_view(&view, &empty_baseline(), empty_policy()) {
        GateResult::NotReady(reason) => assert!(reason.contains("CLOSED"), "{reason}"),
        other => panic!("expected NotReady, got {other:?}"),
    }
}

#[test]
fn evaluate_gh_view_falls_back_to_wallclock_when_merged_at_absent() {
    // gh shouldn't return MERGED without mergedAt, but we don't
    // crash if it does — the wall-clock-now fallback yields a
    // reasonable timestamp. Asserts nonzero rather than a
    // specific value (SystemTime is non-deterministic in tests).
    let view = crate::gh_client::PrView {
        state: "MERGED".to_string(),
        mergeable: "".to_string(),
        status_check_rollup: None,
        merged_at: None,
        base_ref_name: None,
        base_ref_oid: None,
    };
    let baseline = std::collections::HashSet::new();
    let policy = GatePolicy::default_for_merger(0);
    match evaluate_gh_view(&view, &baseline, policy) {
        GateResult::AlreadyMerged { merged_at_ms } => assert!(merged_at_ms > 0),
        other => panic!("expected AlreadyMerged, got {other:?}"),
    }
}

#[test]
fn evaluate_gh_view_routes_closed_state_to_not_ready() {
    // Closed (without merge) is still NotReady — the merger
    // shouldn't try to reconcile a closed-without-merging PR
    // into Merged state. Only MERGED → AlreadyMerged.
    let view = crate::gh_client::PrView {
        state: "CLOSED".to_string(),
        mergeable: "".to_string(),
        status_check_rollup: None,
        merged_at: None,
        base_ref_name: None,
        base_ref_oid: None,
    };
    let baseline = std::collections::HashSet::new();
    let policy = GatePolicy::default_for_merger(0);
    match evaluate_gh_view(&view, &baseline, policy) {
        GateResult::NotReady(reason) => {
            assert!(reason.contains("CLOSED"), "got: {reason}");
        }
        other => panic!("expected NotReady, got {other:?}"),
    }
}

#[test]
fn parse_iso8601_to_ms_handles_standard_gh_format() {
    assert_eq!(
        parse_iso8601_to_ms("2026-06-01T07:04:07Z"),
        Some(1780297447000)
    );
    assert_eq!(
        parse_iso8601_to_ms("2026-06-01T07:04:07.500Z"),
        Some(1780297447500)
    );
    assert_eq!(parse_iso8601_to_ms("garbage"), None);
    assert_eq!(parse_iso8601_to_ms(""), None);
}

#[test]
fn evaluate_gh_view_signals_inherited_with_dedicated_phrase() {
    // The `airc work merge` refusal message points users at the
    // strictly-less-red doctrine; that hint is only actionable if
    // the gate's NotReady reason actually distinguishes inherited
    // failures from new ones with a stable phrase the docs +
    // refusal text both reference.
    let view = parse(serde_json::json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "FAILURE", "status": "COMPLETED", "name": "shell syntax + rust cutover guards"},
            {"conclusion": "FAILURE", "status": "COMPLETED", "name": "cargo test (ubuntu-latest)"},
        ]
    }));
    let baseline = baseline_with(&["shell syntax + rust cutover guards"]);
    let GateResult::NotReady(reason) = evaluate_gh_view(&view, &baseline, empty_policy())
    else {
        panic!("expected NotReady");
    };
    assert!(
        reason.contains("inherited from base, ignored"),
        "phrase must stay stable — `airc work merge` refusal references it: {reason}"
    );
}

// ------------------------------------------------------------------
// Card 7ed1ac4f — pending-too-long timeout
// ------------------------------------------------------------------

/// 2026-05-29T03:29:46Z anchored in ms-since-epoch via the same
/// path airc_lib::gh::client::parse_iso_timestamp_ms uses.
/// Hand-resolving rather than calling the parser keeps the test
/// honest about what the policy actually compares.
const FIXED_STARTED_AT: &str = "2026-05-29T03:29:46Z";
const FIXED_STARTED_AT_MS: u64 = 1_780_025_386_000;

#[test]
fn pending_check_older_than_timeout_counts_as_inherited() {
    // Card 7ed1ac4f core case: a check still IN_PROGRESS for
    // longer than pending_timeout_ms is treated as CI-infra-
    // hung. With everything else green, the PR merges. Mirrors
    // the failure mode observed in #1067+#1070 where Windows
    // runners hung for hours, refusing every PR indefinitely.
    let view = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "status": "COMPLETED", "name": "cargo test (ubuntu-latest)"},
            {"conclusion": null, "status": "IN_PROGRESS",
             "name": "cargo test (windows-latest)",
             "startedAt": FIXED_STARTED_AT},
        ]
    }));
    // now = started + 60min, timeout = 30min → check is 60min old, timed-out.
    let policy = GatePolicy {
        pending_timeout_ms: 30 * 60 * 1000,
        now_ms: FIXED_STARTED_AT_MS + 60 * 60 * 1000,
    };
    assert!(matches!(
        evaluate_gh_view(&view, &empty_baseline(), policy),
        GateResult::Green
    ));
}

#[test]
fn pending_check_younger_than_timeout_still_blocks() {
    // Within the timeout window — still pending, still blocks.
    // The bypass is narrow: only checks past the configured
    // wall-clock age count as "CI hung." A 5-minute pending
    // check is still actively running.
    let view = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "status": "COMPLETED", "name": "cargo test (ubuntu-latest)"},
            {"conclusion": null, "status": "IN_PROGRESS",
             "name": "cargo test (windows-latest)",
             "startedAt": FIXED_STARTED_AT},
        ]
    }));
    let policy = GatePolicy {
        pending_timeout_ms: 30 * 60 * 1000,
        // 5 minutes in — well under the 30-min timeout.
        now_ms: FIXED_STARTED_AT_MS + 5 * 60 * 1000,
    };
    let GateResult::NotReady(reason) = evaluate_gh_view(&view, &empty_baseline(), policy)
    else {
        panic!("expected NotReady");
    };
    assert!(
        reason.contains("1 check(s) still running"),
        "young pending should still block with the unchanged phrase: {reason}"
    );
}

#[test]
fn pending_timeout_zero_keeps_strict_behavior() {
    // Regression: setting the timeout to 0 must NOT silently
    // bypass everything. The default-off mode is "strict gate,
    // no infra-hang bypass" — same as the f16650cd /
    // d5b7b07d era.
    let view = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": null, "status": "IN_PROGRESS",
             "name": "cargo test (windows-latest)",
             "startedAt": FIXED_STARTED_AT},
        ]
    }));
    // Even with `now` an hour past `started`, timeout=0 means
    // no bypass — gate refuses.
    let policy = GatePolicy {
        pending_timeout_ms: 0,
        now_ms: FIXED_STARTED_AT_MS + 60 * 60 * 1000,
    };
    let GateResult::NotReady(reason) = evaluate_gh_view(&view, &empty_baseline(), policy)
    else {
        panic!("expected NotReady");
    };
    assert!(
        reason.contains("still running"),
        "timeout=0 must refuse pending checks regardless of age: {reason}"
    );
}

#[test]
fn new_failure_still_blocks_even_with_inherited_timeouts() {
    // The strictness invariant: a NEW failure (not inherited
    // from base, not a timeout) still blocks even when
    // some other checks are inherited-from-base or timed-out
    // pending. The bypass is per-check, not a free pass.
    let view = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "FAILURE", "status": "COMPLETED", "name": "cargo test (ubuntu-latest)"},
            {"conclusion": null, "status": "IN_PROGRESS",
             "name": "cargo test (windows-latest)",
             "startedAt": FIXED_STARTED_AT},
        ]
    }));
    let policy = GatePolicy {
        pending_timeout_ms: 30 * 60 * 1000,
        now_ms: FIXED_STARTED_AT_MS + 60 * 60 * 1000,
    };
    let GateResult::NotReady(reason) = evaluate_gh_view(&view, &empty_baseline(), policy)
    else {
        panic!("expected NotReady");
    };
    assert!(
        reason.contains("1 failing"),
        "real failure still surfaces even when other checks timed-out: {reason}"
    );
    assert!(
        reason.contains("timed-out pending"),
        "refusal should name the timed-out check separately so the user can see what was bypassed: {reason}"
    );
}

#[test]
fn pending_check_without_started_at_falls_into_active_pending() {
    // Fail-closed bias: if gh somehow returned a pending check
    // with no `startedAt` (older check types, schema drift), we
    // can't compute its age — therefore we MUST treat it as
    // active-pending rather than timing it out. A permissive
    // alternative ("no timestamp = assume old = bypass") would
    // silently merge through any gh schema regression.
    let view = parse(json!({
        "state": "OPEN",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": null, "status": "IN_PROGRESS",
             "name": "cargo test (windows-latest)"},
        ]
    }));
    let policy = GatePolicy {
        pending_timeout_ms: 30 * 60 * 1000,
        now_ms: FIXED_STARTED_AT_MS + 24 * 60 * 60 * 1000,
    };
    assert!(matches!(
        evaluate_gh_view(&view, &empty_baseline(), policy),
        GateResult::NotReady(_)
    ));
}
