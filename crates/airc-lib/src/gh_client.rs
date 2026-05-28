//! Typed `gh` CLI boundary as a trait.
//!
//! Card a094aa81. Card dec35ec7 (#1041) shipped the typed wrapper
//! as a concrete struct inside `airc-cli`. The substrate-vision
//! cards (continuum, openclaw, hermes, codex consumer embedding)
//! need the same boundary without dragging the entire CLI binary
//! along.
//!
//! This module is the consumer-facing half: the [`GhClient`] trait,
//! the typed [`GhError`] enum, the arg/result value types, and the
//! pure parse helpers ([`parse_pr_view`], [`parse_pr_url`]) that
//! describe the shape of `gh`'s output independent of any spawn
//! mechanism. The default shell-based implementation lives in
//! `airc-cli::gh_client::ShellGhClient`; alternative implementations
//! (a mock for tests, an HTTP-direct version when gh's REST surface
//! suffices, a remote-RPC version for headless workers) plug into
//! the same boundary.
//!
//! The split is the same lesson `airc-lib` itself encodes: the
//! consumer-facing API is the trait, the binary owns the
//! implementation, and embedders pick what they want.
//!
//! Note on args ownership: the arg structs ([`PrViewArgs`],
//! [`PrCreateArgs`], …) own their strings (no `&str`/`&Path`
//! borrows). Across an `async fn` call boundary, owning small
//! strings is cheaper than the lifetime gymnastics that
//! `#[async_trait]` imposes on borrowed-arg traits — and it
//! makes mocking trivial.
//!
//! Future cards: see card a094aa81 followups for the mock impl
//! and the codex consumer's HTTP-direct path.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

/// Typed errors from any `gh` operation. Variants are the *actions*
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

/// Typed boundary around GitHub operations the airc CLI and embedded
/// consumers need.
///
/// The trait is dyn-compatible (via `#[async_trait]`) so consumers
/// can store `Arc<dyn GhClient>` without spreading a generic
/// parameter through every callsite. The same shape works for the
/// shell-based implementation in `airc-cli`, mock implementations in
/// tests, and the future HTTP-direct path for headless consumers.
#[async_trait]
pub trait GhClient: Send + Sync {
    /// `gh pr view <number> --repo <owner/name> --json state,mergeable,statusCheckRollup`.
    ///
    /// Used by the continuous-merger (#1037) to gate auto-merge.
    /// `cwd` is the worktree the call should run from — gh resolves
    /// the origin remote from cwd, NOT from `-C` (card a4fe899f).
    async fn pr_view(&self, args: PrViewArgs) -> Result<PrView, GhError>;

    /// `gh pr create --fill --base <branch>` from `cwd`.
    ///
    /// gh's pr-create output is plain text (the PR URL on the last
    /// non-empty line of stdout), not JSON.
    async fn pr_create(&self, args: PrCreateArgs) -> Result<PrCreated, GhError>;

    /// `gh pr merge <number> --repo <owner/name> --squash --delete-branch`.
    /// The merger's terminal call once gate logic passes.
    async fn pr_merge(&self, args: PrMergeArgs) -> Result<MergeReceipt, GhError>;

    /// `gh api -X PATCH repos/{owner}/{repo}/pulls/{number}` with the
    /// supplied `base` ref. Workaround for the Projects-classic
    /// deprecation that breaks `gh pr edit --base` (card 3bf62fbb).
    async fn pr_edit_base(&self, args: PrEditBaseArgs) -> Result<(), GhError>;
}

#[derive(Debug, Clone)]
pub struct PrViewArgs {
    pub repo: String,
    pub number: u64,
    /// Optional cwd. None = process cwd; Some(p) = gh resolves
    /// origin remote from p.
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PrCreateArgs {
    /// Worktree directory the new PR is opened from. gh reads
    /// `git remote get-url origin` here.
    pub cwd: PathBuf,
    /// Integration branch the PR targets. For airc that's
    /// `rust-rewrite` (card 28f1440c).
    pub base: String,
}

#[derive(Debug, Clone)]
pub struct PrMergeArgs {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone)]
pub struct PrEditBaseArgs {
    pub repo: String,
    pub number: u64,
    pub base: String,
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
}
