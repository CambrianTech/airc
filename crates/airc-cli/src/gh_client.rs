// Card a094aa81 split this module:
// - airc-lib::gh_client owns the trait, error enum, arg/result
//   types, and the pure parse helpers — that's the consumer-facing
//   half (Continuum, OpenClaw, Hermes, codex, headless workers).
// - airc-cli::gh_client owns the shell-based implementation — the
//   `tokio::process::Command::new("gh")` spawn lives here only.
//
// Reason for the split: card a094aa81 (substrate-vision consumer
// embedding). Consumers that want the gh boundary without the
// entire CLI binary linked in need a trait-shaped surface; the
// shell impl is the airc CLI's choice of backend, not the
// consumer's obligation.
//
// pr_create / pr_edit_base remain wired in subsequent PRs (the
// merger only consumes pr_view + pr_merge today). The crate-level
// allow is scoped to those known follow-ups.
#![allow(dead_code)]

//! Default shell-based [`GhClient`] implementation backed by the
//! `gh` CLI binary on PATH.
//!
//! The trait and value types are re-exported here from
//! [`airc_lib::gh::client`] so existing crate-internal `use
//! crate::gh_client::PrView` paths continue to work — the types
//! remain a single source of truth in airc-lib; the CLI just
//! pulls them in for its own consumers.

use async_trait::async_trait;
use tokio::process::Command;

pub use airc_lib::gh::client::{
    parse_pr_url, parse_pr_view, BranchCheckRollupArgs, GhCheck, GhClient, GhError, MergeReceipt,
    PrCreateArgs, PrCreated, PrEditBaseArgs, PrMergeArgs, PrView, PrViewArgs,
};
// parse_check_runs is only used inside merger's #[cfg(test)] block — accessed
// via the airc_lib path there to avoid a "unused re-export" lint in non-test
// builds. The shell impl above uses the same path directly.

/// Default [`GhClient`] backed by spawning `gh` as a subprocess.
///
/// Holding a struct rather than free functions is deliberate: it
/// gives a place to add session-scoped state (a cached `gh auth
/// status`, a rate-limit backoff cursor, a shared tokio runtime).
/// Empty today; adding fields here doesn't break callers because
/// `ShellGhClient::default()` is the only ctor.
#[derive(Debug, Default, Clone)]
pub struct ShellGhClient {
    // Reserved for session state.
}

impl ShellGhClient {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl GhClient for ShellGhClient {
    async fn pr_view(&self, args: PrViewArgs) -> Result<PrView, GhError> {
        let mut cmd = Command::new("gh");
        if let Some(ref cwd) = args.cwd {
            cmd.current_dir(cwd);
        }
        let output = cmd
            .args([
                "pr",
                "view",
                &args.number.to_string(),
                "--repo",
                args.repo.as_str(),
                "--json",
                "state,mergeable,statusCheckRollup,mergedAt",
            ])
            .output()
            .await
            .map_err(map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        parse_pr_view(&output.stdout)
    }

    async fn pr_create(&self, args: PrCreateArgs) -> Result<PrCreated, GhError> {
        let output = Command::new("gh")
            .current_dir(&args.cwd)
            .args(["pr", "create", "--fill", "--base", args.base.as_str()])
            .output()
            .await
            .map_err(map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        let stdout = std::str::from_utf8(&output.stdout)
            .map_err(|e| GhError::OutputParse(format!("non-utf8 gh output: {e}")))?;
        parse_pr_url(stdout)
    }

    async fn pr_merge(&self, args: PrMergeArgs) -> Result<MergeReceipt, GhError> {
        let output = Command::new("gh")
            .args([
                "pr",
                "merge",
                &args.number.to_string(),
                "--repo",
                args.repo.as_str(),
                "--squash",
                "--delete-branch",
            ])
            .output()
            .await
            .map_err(map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        Ok(MergeReceipt {
            number: args.number,
            repo: args.repo,
        })
    }

    async fn pr_edit_base(&self, args: PrEditBaseArgs) -> Result<(), GhError> {
        let path = format!("repos/{}/pulls/{}", args.repo, args.number);
        let base_field = format!("base={}", args.base);
        let output = Command::new("gh")
            .args(["api", "-X", "PATCH", &path, "-f", &base_field])
            .output()
            .await
            .map_err(map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        Ok(())
    }

    async fn branch_check_rollup(
        &self,
        args: BranchCheckRollupArgs,
    ) -> Result<Vec<GhCheck>, GhError> {
        // Card d5b7b07d: REST `/check-runs` for the integration branch's
        // HEAD. `--paginate` so a workflow with >30 checks doesn't
        // silently lose the failing ones to pagination. The REST shape
        // is {total_count, check_runs:[...]}; parse_check_runs in
        // airc-lib projects to just the run list.
        let path = format!("repos/{}/commits/{}/check-runs", args.repo, args.branch);
        let output = Command::new("gh")
            .args(["api", "--paginate", &path])
            .output()
            .await
            .map_err(map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        airc_lib::gh::client::parse_check_runs(&output.stdout)
    }
}

/// Classify a non-zero `gh` exit. Substrings are matched against
/// gh's English error messages — if gh's locale or error wording
/// changes, this stops working, which is exactly what we want
/// (silent mis-classification would be worse). Each branch here
/// exists because the calling code has a different action to take.
fn classify_gh_failure(output: &std::process::Output) -> GhError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let lowered = stderr.to_lowercase();
    if lowered.contains("rate limit") || lowered.contains("api rate-limit") {
        GhError::RateLimited { stderr }
    } else if lowered.contains("authentication") || lowered.contains("gh auth login") {
        GhError::AuthRequired { stderr }
    } else if lowered.contains("not a github")
        || lowered.contains("no git remote")
        || lowered.contains("could not determine repository")
    {
        GhError::NotInGithubRepo { stderr }
    } else if lowered.contains("conflict") || lowered.contains("not mergeable") {
        GhError::PrNotMergeable { stderr }
    } else {
        GhError::GhExited {
            code: output.status.code(),
            stderr,
        }
    }
}

/// Map a `tokio::process::Command::output()` io::Error into the
/// typed shape. The "gh not found on PATH" case is special because
/// it's actionable as operator config — distinguish it from a
/// generic process failure.
///
/// `pub(crate)` so the sibling `gh_reqwest` module reuses this for
/// the one gh-process spawn it does (`gh auth token`) — same error
/// surface, no duplication.
pub(crate) fn map_spawn_error(error: std::io::Error) -> GhError {
    if error.kind() == std::io::ErrorKind::NotFound {
        GhError::GhNotFound(error)
    } else {
        GhError::Process(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Card a4fe899f's gh-not-found case (operator missing gh CLI):
    /// distinguish from generic IO failure so the operator gets a
    /// real corrective hint.
    #[test]
    fn map_spawn_error_distinguishes_not_found_from_other_io() {
        let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "gh");
        assert!(matches!(map_spawn_error(not_found), GhError::GhNotFound(_)));

        let permission = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "gh");
        assert!(matches!(map_spawn_error(permission), GhError::Process(_)));
    }

    /// classify_gh_failure pins the substring matches the merger
    /// will rely on. If gh changes its wording, these break loudly
    /// — better than silently bucketing everything into GhExited.
    fn output_with_stderr(stderr: &str) -> std::process::Output {
        std::process::Output {
            status: {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    std::process::ExitStatus::from_raw(256) // non-zero exit
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::ExitStatusExt;
                    std::process::ExitStatus::from_raw(1)
                }
            },
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn classify_rate_limit_message() {
        let err = classify_gh_failure(&output_with_stderr(
            "API rate limit exceeded for installation",
        ));
        assert!(matches!(err, GhError::RateLimited { .. }));
    }

    #[test]
    fn classify_auth_required_message() {
        let err = classify_gh_failure(&output_with_stderr(
            "authentication failed; run `gh auth login`",
        ));
        assert!(matches!(err, GhError::AuthRequired { .. }));
    }

    #[test]
    fn classify_not_in_repo_message() {
        let err = classify_gh_failure(&output_with_stderr(
            "could not determine repository: no git remote",
        ));
        assert!(matches!(err, GhError::NotInGithubRepo { .. }));
    }

    #[test]
    fn classify_merge_conflict_message() {
        let err = classify_gh_failure(&output_with_stderr(
            "Pull request not mergeable: conflicts with base branch",
        ));
        assert!(matches!(err, GhError::PrNotMergeable { .. }));
    }

    #[test]
    fn classify_unknown_message_buckets_to_gh_exited() {
        let err = classify_gh_failure(&output_with_stderr("something went sideways"));
        assert!(matches!(err, GhError::GhExited { .. }));
    }

    // ------------------------------------------------------------------
    // Card 00e3aa39 — measurement establishing the gh-CLI-spawn perf gap.
    //
    // The boundary perf audit (#1077 / #1078 / #1079) found that the
    // substrate's pure-data paths are already fast. The real
    // user-visible latency lives at the gh boundary: every merger
    // tick spawns gh 1+N times (N = Review-state cards with PRs),
    // each ~500-600ms wall-clock.
    //
    // This benchmark MEASURES the gap end-to-end so a future
    // ReqwestGhClient impl has a concrete baseline to beat. Marked
    // `#[ignore]` because it hits the live GitHub API and depends on
    // `gh auth status` succeeding on the runner — not appropriate
    // for CI, but exactly what a perf reviewer wants to run by hand
    // (`cargo test ... --release --ignored -- --nocapture`).
    //
    // The expected delta:
    //   ShellGhClient::branch_check_rollup → ~500-600ms
    //   Direct REST GET via HTTP/2 keep-alive → ~80-120ms (4-7×)
    //
    // The implementation card (00e3aa39 main scope) adds the
    // ReqwestGhClient impl; this bench is its acceptance criterion.
    // ------------------------------------------------------------------

    /// Card 00e3aa39 (with Joel's "public substrate" fix): parameterise
    /// the bench target via env vars so a fork running these benches
    /// hits THEIR repo, not the airc upstream. Skip with a clear
    /// message when unset — the bench is `#[ignore]`'d anyway, but a
    /// fork that opts in via `--ignored` shouldn't accidentally
    /// hammer CambrianTech.
    fn bench_target() -> Option<(String, String)> {
        let repo = std::env::var("AIRC_BENCH_REPO").ok()?;
        let branch = std::env::var("AIRC_BENCH_BRANCH").unwrap_or_else(|_| "main".to_string());
        if repo.is_empty() {
            return None;
        }
        Some((repo, branch))
    }

    #[tokio::test]
    #[ignore = "card 00e3aa39: hits live GitHub API; set AIRC_BENCH_REPO=<owner/name> (and optionally AIRC_BENCH_BRANCH) and run with --ignored"]
    async fn bench_branch_check_rollup_against_live_github() {
        let Some((repo, branch)) = bench_target() else {
            eprintln!(
                "card 00e3aa39: skipping live bench — set AIRC_BENCH_REPO=<owner/name> \
                 (default branch 'main' or override via AIRC_BENCH_BRANCH) to run"
            );
            return;
        };
        let client = ShellGhClient::new();
        let args = BranchCheckRollupArgs {
            repo: repo.clone(),
            branch: branch.clone(),
        };
        eprintln!("card 00e3aa39: target = {repo}@{branch}");

        // Warmup so the first-call DNS / TLS handshake doesn't skew
        // the measured cost. The gh binary has its own auth cache,
        // so this also primes that.
        let _ = client.branch_check_rollup(args.clone()).await;

        const ITERS: u32 = 5;
        let mut total = std::time::Duration::ZERO;
        let mut sample_count = 0usize;
        for i in 0..ITERS {
            let start = std::time::Instant::now();
            let result = client.branch_check_rollup(args.clone()).await;
            let elapsed = start.elapsed();
            total += elapsed;
            match result {
                Ok(runs) => {
                    sample_count = runs.len();
                    eprintln!(
                        "card 00e3aa39: ShellGhClient.branch_check_rollup #{i} → \
                         {elapsed:?} ({} check runs)",
                        runs.len()
                    );
                }
                Err(error) => {
                    eprintln!(
                        "card 00e3aa39: ShellGhClient.branch_check_rollup #{i} \
                         FAILED in {elapsed:?}: {error}"
                    );
                }
            }
        }
        let avg = total / ITERS;
        eprintln!(
            "card 00e3aa39: ShellGhClient AVERAGE over {ITERS} calls: {avg:?} \
             ({sample_count} runs/call). \
             Goal for the ReqwestGhClient follow-up: < {} ms/call.",
            avg.as_millis() / 4
        );

        // Coarse floor: if a single call somehow climbs above 5s
        // we want to know (catastrophic regression — likely network
        // hung). The honest baseline is ~500ms; the floor is loose
        // enough to survive slow runners.
        assert!(
            avg.as_secs() < 5,
            "ShellGhClient regressed to {avg:?} per call — investigate"
        );
    }

    // -----------------------------------------------------------------
    // Card 00e3aa39 Sub-2 — live bench measuring ReqwestGhClient vs
    // ShellGhClient, head-to-head, against the SAME endpoint on the
    // SAME machine. Same env-parameterised target as the Sub-1 bench.
    // -----------------------------------------------------------------

    #[tokio::test]
    #[ignore = "card 00e3aa39 Sub-2: set AIRC_BENCH_REPO + (optionally) AIRC_BENCH_BRANCH and run with --ignored to verify ReqwestGhClient speedup vs ShellGhClient"]
    async fn bench_reqwest_vs_shell_head_to_head() {
        let Some((repo, branch)) = bench_target() else {
            eprintln!(
                "card 00e3aa39 Sub-2: skipping head-to-head bench — set \
                 AIRC_BENCH_REPO=<owner/name> to run"
            );
            return;
        };
        let shell = ShellGhClient::new();
        let reqw = crate::gh_reqwest::ReqwestGhClient::new()
            .expect("reqwest client builds — rustls-tls feature is enabled in Cargo.toml");
        let args = BranchCheckRollupArgs {
            repo: repo.clone(),
            branch: branch.clone(),
        };
        eprintln!("card 00e3aa39 Sub-2: target = {repo}@{branch}");

        // Warmup both — DNS, TLS handshake, gh-auth-token cache.
        let _ = shell.branch_check_rollup(args.clone()).await;
        let _ = reqw.branch_check_rollup(args.clone()).await;

        const ITERS: u32 = 5;

        let mut shell_total = std::time::Duration::ZERO;
        for i in 0..ITERS {
            let start = std::time::Instant::now();
            let result = shell.branch_check_rollup(args.clone()).await;
            let elapsed = start.elapsed();
            shell_total += elapsed;
            match result {
                Ok(runs) => eprintln!(
                    "card 00e3aa39 Sub-2: ShellGhClient #{i} → {elapsed:?} ({} runs)",
                    runs.len()
                ),
                Err(e) => {
                    eprintln!("card 00e3aa39 Sub-2: ShellGhClient #{i} FAILED in {elapsed:?}: {e}")
                }
            }
        }
        let shell_avg = shell_total / ITERS;

        let mut reqw_total = std::time::Duration::ZERO;
        for i in 0..ITERS {
            let start = std::time::Instant::now();
            let result = reqw.branch_check_rollup(args.clone()).await;
            let elapsed = start.elapsed();
            reqw_total += elapsed;
            match result {
                Ok(runs) => eprintln!(
                    "card 00e3aa39 Sub-2: ReqwestGhClient #{i} → {elapsed:?} ({} runs)",
                    runs.len()
                ),
                Err(e) => eprintln!(
                    "card 00e3aa39 Sub-2: ReqwestGhClient #{i} FAILED in {elapsed:?}: {e}"
                ),
            }
        }
        let reqw_avg = reqw_total / ITERS;

        let speedup = shell_avg.as_secs_f64() / reqw_avg.as_secs_f64().max(0.0001);
        eprintln!(
            "card 00e3aa39 Sub-2 HEAD-TO-HEAD AVERAGE over {ITERS} calls each: \
             Shell={shell_avg:?}  Reqwest={reqw_avg:?}  speedup={speedup:.2}×"
        );

        // Honest floor (per session conversation): ReqwestGhClient
        // must be at least 1.2× faster than ShellGhClient. The
        // original 4× projection was overstated — GitHub's REST API
        // is ~400ms regardless of caller; the real lever is GraphQL
        // batching, carded as Sub-3.
        let one_two_x_floor = reqw_avg.as_secs_f64() * 1.2 <= shell_avg.as_secs_f64();
        assert!(
            one_two_x_floor,
            "ReqwestGhClient did not clear the 1.2× honest floor: \
             Shell={shell_avg:?}  Reqwest={reqw_avg:?}  speedup={speedup:.2}×. \
             Sub-3 (GraphQL batching) is where the 4× actually lives — see the card."
        );
    }
}
