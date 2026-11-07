//! The merge gate's decision over one PR: its live state, its checks against the base's own
//! failures (strictly less red, card d5b7b07d), and the baseline cache those reads share.

use super::*;

/// Result of applying the merge gate to a PR. `pub(crate)` since card
/// a399b342: the `airc work merge` CLI command consumes the same gate
/// so a manual merge is held to the same bar the auto-merger uses.
#[derive(Debug)]
pub(crate) enum GateResult {
    Green,
    NotReady(String),
    /// Card acd72c81 follow-up: PR's GitHub state is already MERGED
    /// (someone merged it out-of-band, or the merger crashed after
    /// `gh pr merge` but before `mark_pull_request_merged`, or this
    /// card was created post-hoc to track an already-shipped PR).
    /// The card needs `PullRequestMerged` to reach the Merged state,
    /// but `gh pr merge` would 422. `merged_at_ms` is parsed from
    /// gh's `mergedAt`; falls back to wall-clock-now if absent.
    AlreadyMerged {
        merged_at_ms: u64,
    },
}

/// Card 7ed1ac4f — tunable policy parameters carried into
/// [`evaluate_gh_view`]. Kept as a plain Copy struct so the pure
/// function stays trivially testable. The defaults
/// ([`Self::default_for_merger`]) preserve the f16650cd / d5b7b07d
/// behavior — pending_timeout_ms > 0 only when the CLI explicitly
/// opts in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GatePolicy {
    /// Pending check age (ms) past which the gate treats the check
    /// as "inherited from base" (CI hung, not test red). `0` =
    /// disabled (strict pre-7ed1ac4f behavior).
    pub(crate) pending_timeout_ms: u64,
    /// Current time in ms-since-epoch. Injected for testability; the
    /// IO path fills this with `SystemTime::now()`.
    pub(crate) now_ms: u64,
}

impl GatePolicy {
    /// Default for the continuous merger / `airc work merge` CLI:
    /// 30-minute pending timeout. Matches the observation window
    /// for the Windows-runner hang failure mode (3-8 min normal,
    /// 30+ min hung). Callers override via the CLI flag.
    pub(crate) fn default_for_merger(now_ms: u64) -> Self {
        Self {
            pending_timeout_ms: 30 * 60 * 1000,
            now_ms,
        }
    }
}

/// Snapshot wall-clock ms — once per tick, plumbed through the gate
/// for testable determinism.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Parse gh's `mergedAt` (ISO-8601, e.g. "2026-06-01T07:04:07Z") to
/// epoch milliseconds. Returns `None` on parse failure so the caller
/// can fall back to wall-clock-now. Card acd72c81 follow-up: the
/// reconcile path's canonical timestamp is GitHub's recorded merge
/// time, not the reconcile tick's wall clock.
pub(crate) fn parse_iso8601_to_ms(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis().max(0) as u64)
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
pub(crate) async fn check_pr_gate(
    gh: &dyn crate::gh_client::GhClient,
    pr: &airc_work::model::PullRequestRef,
    baselines: &mut BaselineCache,
    policy: GatePolicy,
) -> Result<GateResult, crate::gh_client::GhError> {
    let view = gh
        .pr_view(crate::gh_client::PrViewArgs {
            repo: pr.repo.as_str().to_string(),
            number: pr.number,
            cwd: None,
        })
        .await?;
    let no_allowance = HashSet::new();
    let without_baseline = evaluate_gh_view(&view, &no_allowance, policy);
    if !matches!(without_baseline, GateResult::NotReady(_)) {
        // A decision needing no base allowance (including the configured
        // pending-timeout policy) does not depend on another lookup succeeding.
        return Ok(without_baseline);
    }
    let Some(target) = BaselineTarget::from_view(&pr.repo, &view) else {
        eprintln!(
            "airc-merger: PR #{} in {} has no verified target branch/revision \
             (degrading to no-allowance gate)",
            pr.number, pr.repo
        );
        return Ok(without_baseline);
    };
    let failures = match baselines.failures.entry(target) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let failures = fetch_baseline_failures(gh, entry.key()).await;
            entry.insert(failures)
        }
    };
    Ok(evaluate_gh_view(&view, failures, policy))
}

/// Short-lived baseline observations for one merger tick or manual command.
/// No cross-repository, cross-branch, or cross-revision failure allowance.
#[derive(Default)]
pub(crate) struct BaselineCache {
    failures: HashMap<BaselineTarget, HashSet<String>>,
}

#[derive(PartialEq, Eq, Hash)]
struct BaselineTarget {
    repo: airc_work::ids::RepoId,
    branch: airc_work::model::BranchName,
    revision: airc_work::model::GitObjectId,
}

impl BaselineTarget {
    fn from_view(repo: &airc_work::ids::RepoId, view: &crate::gh_client::PrView) -> Option<Self> {
        let branch = airc_work::model::BranchName::new(view.base_ref_name.as_deref()?).ok()?;
        let revision = airc_work::model::GitObjectId::new(view.base_ref_oid.as_deref()?).ok()?;
        // GitHub supplies a complete object id. A short or malformed value
        // must never fall back to resolving a mutable branch or ambiguous ref.
        if !matches!(revision.as_str().len(), 40 | 64) {
            return None;
        }
        Some(Self {
            repo: repo.clone(),
            branch,
            revision,
        })
    }
}

/// Query the target revision from the same live PR response as its checks.
/// Lookup failure is cached as an empty allowance for this tick; failures in
/// another repository or branch can never supply the missing evidence.
async fn fetch_baseline_failures(
    gh: &dyn crate::gh_client::GhClient,
    target: &BaselineTarget,
) -> HashSet<String> {
    let runs = match gh
        .branch_check_rollup(crate::gh_client::BranchCheckRollupArgs {
            repo: target.repo.as_str().to_string(),
            branch: target.revision.as_str().to_string(),
        })
        .await
    {
        Ok(runs) => runs,
        Err(error) => {
            eprintln!(
                "airc-merger: baseline lookup failed for {}:{} at {}: {error} \
                 (degrading to no-allowance gate)",
                target.repo,
                target.branch.as_str(),
                target.revision.as_str()
            );
            return std::collections::HashSet::new();
        }
    };
    let failures = baseline_failing_names(&runs);
    if !failures.is_empty() {
        eprintln!(
            "airc-merger: baseline {}:{} at {} has {} failing check(s) — \
             only this target's inherited failures are ignored",
            target.repo,
            target.branch.as_str(),
            target.revision.as_str(),
            failures.len()
        );
    }
    failures
}

/// Pure projection: rollup → set of check NAMES whose conclusion is
/// failure-shaped (case-insensitive). Kept separate from the IO half
/// so the strictly-less-red logic is unit-testable without a live
/// `gh`.
pub(crate) fn baseline_failing_names(
    runs: &[crate::gh_client::GhCheck],
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for c in current_runs(runs) {
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

/// The runs that speak for a revision: a check re-run on the same head
/// supersedes its earlier runs of the same name, the rule GitHub's own
/// required checks apply. Without it, a force-push's cancelled runs stayed
/// "failing" beside their green successors forever (airc #1539: 13 cancelled,
/// all 15 latest green, merge refused). Fail-closed: a run is dropped only
/// for a same-named run that provably started later; a missing name or
/// timestamp, or a tie, keeps both.
fn current_runs(
    runs: &[crate::gh_client::GhCheck],
) -> impl Iterator<Item = &crate::gh_client::GhCheck> {
    let started = |c: &crate::gh_client::GhCheck| {
        c.started_at
            .as_deref()
            .and_then(airc_lib::gh::client::parse_iso_timestamp_ms)
    };
    runs.iter().filter(move |earlier| {
        !runs.iter().any(|later| {
            match (
                later.name.as_deref(),
                earlier.name.as_deref(),
                started(later),
                started(earlier),
            ) {
                (Some(a), Some(b), Some(l), Some(e)) => a == b && l > e,
                _ => false,
            }
        })
    })
}

/// Decide whether a PR is ready to merge, given the parsed `gh pr
/// view` payload. Pure — no IO, no async.
///
/// CONTRACT (card fc483e57): this function NEVER sees dialect. Its
/// [`PrView`] comes from `PrView::from_graphql` / `PrView::from_rest`,
/// which emit the canonical (GraphQL) vocabulary — so `MERGED`,
/// `SUCCESS`, `COMPLETED` are matched as written, and a REST/GraphQL
/// difference is fixed at that boundary, never here. Do not add a
/// `to_uppercase()` or a `state == "closed"` arm below; add it to
/// `canonicalize`.
///
/// First-cut policy:
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
///
/// Card 7ed1ac4f adds the pending-too-long timeout: a check still
/// `IN_PROGRESS` / `QUEUED` after [`GatePolicy::pending_timeout_ms`]
/// counts as inherited (CI infrastructure hang, not test red). This
/// closes the dogfood-blocking failure mode observed in #1067+#1070
/// where Windows runners hung for hours, refusing every PR merge
/// indefinitely.
pub(crate) fn evaluate_gh_view(
    view: &crate::gh_client::PrView,
    baseline_failures: &std::collections::HashSet<String>,
    policy: GatePolicy,
) -> GateResult {
    // Card c03bb62f: "merged" is a FACT (`merged_at` is set), not a state
    // string. GraphQL (`gh pr view`) reports `state: MERGED`; REST
    // (`/repos/../pulls/N`, the production ReqwestGhClient) reports
    // `state: closed` + `merged_at` for the same PR — so a string-only
    // check let every merged PR read as CLOSED and skipped the reconcile,
    // leaving cards at Review forever (2026-09-04: three continuum cards
    // whose PRs merged on GitHub, `airc work merge` refusing "state is
    // CLOSED"). Decide on the fact; the state string is a hint.
    if view.state == "MERGED" || view.merged_at.is_some() {
        // Card acd72c81 follow-up: reconcile, don't skip. The kanban
        // projection needs `PullRequestMerged` to transition; the GH
        // merge already happened, so the merger just emits the event.
        let merged_at_ms = view
            .merged_at
            .as_deref()
            .and_then(parse_iso8601_to_ms)
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            });
        return GateResult::AlreadyMerged { merged_at_ms };
    }
    if view.state != "OPEN" {
        return GateResult::NotReady(format!("PR state is {} (not OPEN)", view.state));
    }
    if view.mergeable == "CONFLICTING" {
        return GateResult::NotReady("PR has merge conflicts; needs rebase".to_string());
    }
    let rollup = view.status_check_rollup.as_deref().unwrap_or(&[]);
    let mut new_failures = 0usize;
    let mut inherited_failures = 0usize;
    let mut active_pending = 0usize;
    let mut timed_out_pending = 0usize;
    for c in current_runs(rollup) {
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
                    // Card 7ed1ac4f: a check pending longer than the
                    // configured timeout is treated as inherited
                    // (CI hung, not test red). 0 disables the
                    // bypass — strict-gate behaviour for callers
                    // that want it. Missing started_at falls into
                    // the active-pending bucket — the fail-closed
                    // bias: if we can't compute age, we don't bypass.
                    let timed_out = policy.pending_timeout_ms > 0
                        && c.started_at
                            .as_deref()
                            .and_then(airc_lib::gh::client::parse_iso_timestamp_ms)
                            .map(|started| {
                                policy.now_ms.saturating_sub(started) > policy.pending_timeout_ms
                            })
                            .unwrap_or(false);
                    if timed_out {
                        timed_out_pending += 1;
                    } else {
                        active_pending += 1;
                    }
                }
            }
        }
    }
    if new_failures > 0 {
        let mut notes = Vec::new();
        if inherited_failures > 0 {
            notes.push(format!("{inherited_failures} inherited from base, ignored"));
        }
        if timed_out_pending > 0 {
            notes.push(format!(
                "{timed_out_pending} timed-out pending, ignored (CI hung)"
            ));
        }
        let note = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join("; "))
        };
        return GateResult::NotReady(format!("{new_failures} failing check(s){note}"));
    }
    if active_pending > 0 {
        let timeout_note = if timed_out_pending > 0 {
            format!(
                " ({timed_out_pending} other check(s) timed-out pending, ignored \
                 — only these {active_pending} still actively running)"
            )
        } else {
            String::new()
        };
        return GateResult::NotReady(format!(
            "{active_pending} check(s) still running{timeout_note}"
        ));
    }
    GateResult::Green
}
