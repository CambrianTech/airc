// Card dec35ec7 ships the substrate; the merger consumes pr_view +
// pr_merge in this PR. The remaining methods (pr_create, pr_edit_base,
// parse_pr_url, the OutputParse error variant, the pr_create-shaped
// argument types) are wired into work_commands.rs as a follow-up
// (#1041) so this PR doesn't conflict with peer's c0bd865c
// (work_commands split). Each is dead-code today; the allowlist is
// per-module and scoped to that explicit follow-up.
#![allow(dead_code)]

//! Typed wrapper around the `gh` CLI.
//!
//! Card dec35ec7. Before this module, every airc-cli command that
//! talked to GitHub built its own `tokio::process::Command::new("gh")`,
//! parsed stderr into a human string, decoded stdout via inline
//! `serde_json::from_slice`, and returned `Box<dyn std::error::Error>`.
//! 21+ sites across the workspace; three of them lived in
//! `work_commands.rs` alone, none sharing helpers.
//!
//! Net effect: every consumer (continuum, openclaw, hermes, codex)
//! looking to embed airc had to re-implement the same gh
//! orchestration in their language, or fork the airc CLI per
//! operation. Neither is what the substrate is for.
//!
//! [`GhClient`] is the typed boundary. Callers get:
//!
//! - **Typed responses** ([`PrView`], [`PrCreated`], [`MergeReceipt`])
//!   instead of raw JSON `Value`.
//! - **Typed errors** ([`GhError`]) instead of `Box<dyn Error>`. The
//!   merger can match `GhError::RateLimited` and back off; the
//!   close-guard can match `GhError::PrConflicts` and refuse rather
//!   than swallowing.
//! - **One place** for the "set cwd, not `gh -C`" lesson (card
//!   a4fe899f), for the rate-limit detection logic, for the JSON
//!   parsing that has to evolve as gh's schema does.
//! - **Pure parse helpers** ([`parse_pr_view`], [`parse_pr_url`])
//!   that don't need a real `gh` to test — synthetic JSON in,
//!   typed value out.
//!
//! Future cards expand the surface (gh issue, gh repo view's other
//! fields, gh api PATCH for the body-edit workaround tracked as
//! 3bf62fbb). This module is the place to add them.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;
use tokio::process::Command;

/// Typed errors from any `gh` invocation. Variants are the *actions*
/// a caller can take in response, not the exit codes — so a future
/// migration to `gh api`'s structured error shape doesn't churn the
/// downstream `match`es.
#[derive(Debug, Error)]
pub enum GhError {
    /// `gh` binary not on PATH or not executable. Operator config
    /// problem; nothing to retry.
    #[error("gh binary not found on PATH: {0}")]
    GhNotFound(std::io::Error),

    /// `gh auth status` failed when the call attempted authentication.
    /// Operator needs `gh auth login`.
    #[error("gh authentication required: run `gh auth login` ({stderr})")]
    AuthRequired { stderr: String },

    /// `gh` ran but the cwd isn't a github checkout (no `origin`
    /// remote, non-github origin, etc.). Card 59243bee already gates
    /// some of this on the airc side; this is the gh-side surface.
    #[error("not in a github checkout: {stderr}")]
    NotInGithubRepo { stderr: String },

    /// Hit GitHub's rate limit. Callers should back off and retry.
    /// The merger's tick failure path should match this and skip
    /// rather than logging an opaque "tick failed".
    #[error("github rate limit reached: {stderr}")]
    RateLimited { stderr: String },

    /// The PR exists but cannot be merged in its current state —
    /// conflicts, branch protection failure, or non-OPEN state.
    /// Distinct from a JSON-parse failure so the merger can hold
    /// the card in Review and wait.
    #[error("pr not mergeable: {stderr}")]
    PrNotMergeable { stderr: String },

    /// Catch-all for `gh` exiting non-zero in a way we haven't
    /// classified yet. Includes the stderr so the operator can act,
    /// but downstream code should NOT pattern-match on the string —
    /// upgrade to a typed variant if the case recurs.
    #[error("gh exited {code:?}: {stderr}")]
    GhExited { code: Option<i32>, stderr: String },

    /// `gh`'s JSON output didn't deserialize. Either a gh schema
    /// drift or we passed the wrong --json fields. Bug, not runtime
    /// condition.
    #[error("could not parse gh json output: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// `gh`'s stdout was the wrong shape (e.g. expected a PR URL on
    /// the last non-empty line, got something else). Distinct from
    /// `JsonParse` because gh's pr-create output is plain text, not
    /// JSON.
    #[error("could not parse gh output: {0}")]
    OutputParse(String),

    /// Process management failed (couldn't spawn, couldn't read
    /// stdout). Underlying io::Error included.
    #[error("gh process failed: {0}")]
    Process(#[from] std::io::Error),
}

/// Stateless typed wrapper around `gh`. Held by the CLI for the
/// session; methods are `&self` so callers can share one instance.
///
/// Holding a wrapper rather than a free function is deliberate: it
/// gives a place to add session-scoped state (a cached `gh auth
/// status`, a rate-limit backoff cursor, a shared tokio runtime) and
/// makes mocking possible in tests without polluting every callsite
/// with a generic parameter.
#[derive(Debug, Default, Clone)]
pub struct GhClient {
    // Reserved for session state. Empty struct today; adding fields
    // here doesn't break callers because `GhClient::default()` is
    // the only ctor.
}

impl GhClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// `gh pr view <number> --repo <owner/name> --json state,mergeable,statusCheckRollup`.
    ///
    /// Used by the continuous-merger (#1037) to gate auto-merge.
    /// `cwd` is the worktree the call should run from — gh resolves
    /// the origin remote from cwd, NOT from `-C` (card a4fe899f).
    pub async fn pr_view(&self, args: PrViewArgs<'_>) -> Result<PrView, GhError> {
        let mut cmd = Command::new("gh");
        if let Some(cwd) = args.cwd {
            cmd.current_dir(cwd);
        }
        let output = cmd
            .args([
                "pr",
                "view",
                &args.number.to_string(),
                "--repo",
                args.repo,
                "--json",
                "state,mergeable,statusCheckRollup",
            ])
            .output()
            .await
            .map_err(self::map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        parse_pr_view(&output.stdout)
    }

    /// `gh pr create --fill --base <branch>` from `cwd`.
    ///
    /// gh's pr-create output is plain text (the PR URL on the last
    /// non-empty line of stdout), not JSON. Card 13131f1c tracks
    /// the title-derivation issue; not this module's concern.
    pub async fn pr_create(&self, args: PrCreateArgs<'_>) -> Result<PrCreated, GhError> {
        let output = Command::new("gh")
            .current_dir(args.cwd)
            .args(["pr", "create", "--fill", "--base", args.base])
            .output()
            .await
            .map_err(self::map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        let stdout = std::str::from_utf8(&output.stdout)
            .map_err(|e| GhError::OutputParse(format!("non-utf8 gh output: {e}")))?;
        parse_pr_url(stdout)
    }

    /// `gh pr merge <number> --repo <owner/name> --squash --delete-branch`.
    /// The merger's terminal call once gate logic passes.
    pub async fn pr_merge(&self, args: PrMergeArgs<'_>) -> Result<MergeReceipt, GhError> {
        let output = Command::new("gh")
            .args([
                "pr",
                "merge",
                &args.number.to_string(),
                "--repo",
                args.repo,
                "--squash",
                "--delete-branch",
            ])
            .output()
            .await
            .map_err(self::map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        Ok(MergeReceipt {
            number: args.number,
            repo: args.repo.to_string(),
        })
    }

    /// `gh api -X PATCH repos/{owner}/{repo}/pulls/{number}` with the
    /// supplied `base` ref. Workaround for the Projects-classic
    /// deprecation that breaks `gh pr edit --base` (card 3bf62fbb).
    /// The plain `gh pr edit` path emits a hard error from the
    /// GraphQL projectCards subselection; the REST PATCH route is
    /// the documented workaround until gh upgrades.
    pub async fn pr_edit_base(&self, args: PrEditBaseArgs<'_>) -> Result<(), GhError> {
        let path = format!("repos/{}/pulls/{}", args.repo, args.number);
        let base_field = format!("base={}", args.base);
        let output = Command::new("gh")
            .args(["api", "-X", "PATCH", &path, "-f", &base_field])
            .output()
            .await
            .map_err(self::map_spawn_error)?;
        if !output.status.success() {
            return Err(classify_gh_failure(&output));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PrViewArgs<'a> {
    pub repo: &'a str,
    pub number: u64,
    /// Optional cwd. None = process cwd; Some(p) = gh resolves
    /// origin remote from p.
    pub cwd: Option<&'a Path>,
}

#[derive(Debug, Clone)]
pub struct PrCreateArgs<'a> {
    /// Worktree directory the new PR is opened from. gh reads
    /// `git remote get-url origin` here.
    pub cwd: &'a Path,
    /// Integration branch the PR targets. For airc that's
    /// `rust-rewrite` (card 28f1440c).
    pub base: &'a str,
}

#[derive(Debug, Clone)]
pub struct PrMergeArgs<'a> {
    pub repo: &'a str,
    pub number: u64,
}

#[derive(Debug, Clone)]
pub struct PrEditBaseArgs<'a> {
    pub repo: &'a str,
    pub number: u64,
    pub base: &'a str,
}

/// `gh pr view --json state,mergeable,statusCheckRollup` shape.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PrView {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub mergeable: String,
    #[serde(default, rename = "statusCheckRollup")]
    pub status_check_rollup: Option<Vec<GhCheck>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GhCheck {
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCreated {
    pub url: String,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReceipt {
    pub repo: String,
    pub number: u64,
}

// --- pure parsers (testable without spawning gh) ---

/// Decode the JSON body of `gh pr view --json ...` into [`PrView`].
/// Pure — synthetic JSON in, typed value out. The merger and the
/// close-guard both depend on this shape, so this is where the
/// schema round-trip is pinned in tests.
pub fn parse_pr_view(json: &[u8]) -> Result<PrView, GhError> {
    Ok(serde_json::from_slice(json)?)
}

/// Extract the PR URL + number from `gh pr create`'s plain-text
/// stdout. gh prints the URL on the last non-empty line; the number
/// is the trailing path segment.
///
/// Returns [`GhError::OutputParse`] when the shape doesn't match —
/// callers should NOT fall back to "extract from any line that looks
/// URL-shaped" (gh's output evolves; bug surface should surface).
pub fn parse_pr_url(stdout: &str) -> Result<PrCreated, GhError> {
    let url = stdout
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .ok_or_else(|| GhError::OutputParse("gh pr create produced no output lines".into()))?;
    let number = url
        .rsplit('/')
        .next()
        .and_then(|tail| tail.parse::<u64>().ok())
        .ok_or_else(|| {
            GhError::OutputParse(format!(
                "could not extract PR number from gh output line: {url:?}"
            ))
        })?;
    Ok(PrCreated {
        url: url.to_string(),
        number,
    })
}

/// Classify a non-zero `gh` exit. The substrings are intentionally
/// matched against gh's English error messages — if gh's locale or
/// error wording changes, this stops working, which is exactly what
/// we want (silent mis-classification would be worse). Each branch
/// here exists because the calling code has a different action to
/// take per case.
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
fn map_spawn_error(error: std::io::Error) -> GhError {
    if error.kind() == std::io::ErrorKind::NotFound {
        GhError::GhNotFound(error)
    } else {
        GhError::Process(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_view_decodes_all_fields() {
        let json = br#"{
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [
                {"name": "fmt",     "status": "COMPLETED",  "conclusion": "SUCCESS"},
                {"name": "tests",   "status": "IN_PROGRESS", "conclusion": null},
                {"name": "clippy",  "status": "COMPLETED",  "conclusion": "FAILURE"}
            ]
        }"#;
        let view = parse_pr_view(json).expect("parse");
        assert_eq!(view.state, "OPEN");
        assert_eq!(view.mergeable, "MERGEABLE");
        let checks = view.status_check_rollup.unwrap();
        assert_eq!(checks.len(), 3);
        assert_eq!(
            checks[1].conclusion, None,
            "in-flight check has null conclusion"
        );
    }

    #[test]
    fn parse_pr_view_tolerates_missing_optional_fields() {
        let json = br#"{}"#;
        let view = parse_pr_view(json).expect("empty object decodes");
        assert!(view.state.is_empty());
        assert!(view.mergeable.is_empty());
        assert!(view.status_check_rollup.is_none());
    }

    #[test]
    fn parse_pr_url_extracts_last_url_and_number() {
        let stdout = "Creating draft pull request for refs/heads/feat/x into rust-rewrite\n\
                      https://github.com/CambrianTech/airc/pull/1038\n";
        let created = parse_pr_url(stdout).expect("parse");
        assert_eq!(created.number, 1038);
        assert_eq!(
            created.url,
            "https://github.com/CambrianTech/airc/pull/1038"
        );
    }

    #[test]
    fn parse_pr_url_rejects_unparseable_tail() {
        let stdout = "https://github.com/owner/repo/pull/not-a-number\n";
        assert!(matches!(parse_pr_url(stdout), Err(GhError::OutputParse(_))));
    }

    #[test]
    fn parse_pr_url_rejects_empty_output() {
        assert!(matches!(parse_pr_url(""), Err(GhError::OutputParse(_))));
        assert!(matches!(
            parse_pr_url("\n\n  \n"),
            Err(GhError::OutputParse(_))
        ));
    }

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
}
