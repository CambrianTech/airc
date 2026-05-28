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
//! [`airc_lib::gh_client`] so existing crate-internal `use
//! crate::gh_client::PrView` paths continue to work — the types
//! remain a single source of truth in airc-lib; the CLI just
//! pulls them in for its own consumers.

use async_trait::async_trait;
use tokio::process::Command;

pub use airc_lib::gh_client::{
    parse_pr_url, parse_pr_view, GhClient, GhError, MergeReceipt, PrCreateArgs, PrCreated,
    PrEditBaseArgs, PrMergeArgs, PrView, PrViewArgs,
};

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
                "state,mergeable,statusCheckRollup",
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
