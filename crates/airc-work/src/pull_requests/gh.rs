//! Real GitHub source for [`PullRequestSource`].
//!
//! Shells out to `gh` (the GitHub CLI) and translates the JSON output
//! into the [`RepoPullRequestSnapshot`] / [`PullRequestSnapshot`] shape
//! the substrate diff function consumes. Behind a [`GhCommandRunner`]
//! trait so tests stub the I/O surface without spawning processes
//! (same shape as [`crate::local_git::GitCommandRunner`]).
//!
//! Scope: this PR ships **check state + merge state** translation.
//! Review-state translation is intentionally not wired here because it
//! needs an answer for the GitHub-login → `PeerId` mapping question —
//! the existing `PullRequestSnapshot::reviews` field uses cryptographic
//! `PeerId`s, but `gh` returns GitHub usernames. Real cross-walk needs
//! its own design pass. Until then, `reviews` is always empty when this
//! source emits — the diff function still works, it just doesn't emit
//! `PullRequestReviewSubmitted` events from this path yet.

use std::collections::HashMap;
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::{
    PullRequestSnapshot, PullRequestSource, PullRequestSourceError, RepoPullRequestSnapshot,
};
use crate::model::{BranchName, PrCheckState, PrMergeState, PullRequestRef};
use crate::RepoId;

/// I/O abstraction for the `gh` CLI. Production uses
/// [`CommandGhRunner`]; tests stub with canned JSON strings.
pub trait GhCommandRunner {
    fn run_gh(&self, args: &[&str]) -> Result<String, GhRunnerError>;
}

#[derive(Debug, thiserror::Error)]
pub enum GhRunnerError {
    #[error("failed to spawn gh: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("gh exited non-zero: status={status:?}, stderr={stderr}")]
    NonZero { status: Option<i32>, stderr: String },
}

/// Real `gh` runner — spawns the `gh` binary and returns its stdout.
/// `gh` resolves auth from the user's existing keychain/config; this
/// source does not own credential plumbing.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandGhRunner;

impl GhCommandRunner for CommandGhRunner {
    fn run_gh(&self, args: &[&str]) -> Result<String, GhRunnerError> {
        let output = Command::new("gh").args(args).output()?;
        if !output.status.success() {
            return Err(GhRunnerError::NonZero {
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// PR source backed by `gh pr list`. Returns a snapshot of every open
/// PR on `repo` with check and merge state translated to the
/// substrate's typed enums.
#[derive(Debug, Clone)]
pub struct GhPullRequestSource<R = CommandGhRunner> {
    runner: R,
}

impl Default for GhPullRequestSource<CommandGhRunner> {
    fn default() -> Self {
        Self::new(CommandGhRunner)
    }
}

impl<R: GhCommandRunner> GhPullRequestSource<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }
}

/// `gh pr list --json` fields the source pulls. Documented separately
/// so test fixtures can mirror them exactly.
pub const GH_JSON_FIELDS: &str =
    "number,headRefName,baseRefName,state,isDraft,mergeStateStatus,statusCheckRollup";

impl<R: GhCommandRunner> PullRequestSource for GhPullRequestSource<R> {
    fn snapshot(&self, repo: &RepoId) -> Result<RepoPullRequestSnapshot, PullRequestSourceError> {
        // `--state all` so we observe merged/closed transitions —
        // diff function decides what's a change vs. steady state.
        let stdout = self
            .runner
            .run_gh(&[
                "pr",
                "list",
                "--repo",
                repo.as_str(),
                "--state",
                "all",
                "--json",
                GH_JSON_FIELDS,
            ])
            .map_err(|error| PullRequestSourceError::Source(error.to_string()))?;

        let rows: Vec<GhPullRequestRow> = serde_json::from_str(&stdout)
            .map_err(|error| PullRequestSourceError::Source(format!("gh json: {error}")))?;

        let mut pulls = HashMap::with_capacity(rows.len());
        for row in rows {
            let snapshot = row.into_snapshot(repo.clone())?;
            pulls.insert(snapshot.pull_request.number, snapshot);
        }

        Ok(RepoPullRequestSnapshot {
            repo: repo.clone(),
            pulls,
        })
    }
}

/// Raw JSON shape from `gh pr list --json`. Field names match `gh`'s
/// output exactly so deserialization is one-shot.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct GhPullRequestRow {
    number: u64,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    /// `OPEN` / `MERGED` / `CLOSED`.
    state: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    /// `CLEAN` / `BLOCKED` / `DIRTY` / `HAS_HOOKS` / `UNSTABLE` /
    /// `UNKNOWN`. Only meaningful when `state == "OPEN"`.
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<GhCheckRow>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct GhCheckRow {
    /// `QUEUED` / `IN_PROGRESS` / `COMPLETED` (for newer check-suite
    /// rows) or absent (older status-context rows expose only
    /// `state`).
    #[serde(default)]
    status: Option<String>,
    /// `SUCCESS` / `FAILURE` / `CANCELLED` / `SKIPPED` / `NEUTRAL` /
    /// `TIMED_OUT` / `ACTION_REQUIRED` / `STARTUP_FAILURE`. Set only
    /// when `status == "COMPLETED"`.
    #[serde(default)]
    conclusion: Option<String>,
    /// Legacy `state` field for status-context rows: `PENDING` /
    /// `SUCCESS` / `FAILURE` / `ERROR`.
    #[serde(default)]
    state: Option<String>,
}

impl GhPullRequestRow {
    fn into_snapshot(self, repo: RepoId) -> Result<PullRequestSnapshot, PullRequestSourceError> {
        let head = BranchName::new(self.head_ref_name)
            .map_err(|error| PullRequestSourceError::Source(format!("head ref: {error}")))?;
        let base = BranchName::new(self.base_ref_name)
            .map_err(|error| PullRequestSourceError::Source(format!("base ref: {error}")))?;

        let merge_state = derive_merge_state(
            &self.state,
            self.is_draft,
            self.merge_state_status.as_deref(),
        );
        let check_state = derive_check_state(&self.status_check_rollup);

        Ok(PullRequestSnapshot {
            pull_request: PullRequestRef {
                repo,
                number: self.number,
                head,
                base,
            },
            check_state,
            merge_state,
            reviews: HashMap::new(),
        })
    }
}

fn derive_merge_state(
    state: &str,
    is_draft: bool,
    merge_state_status: Option<&str>,
) -> PrMergeState {
    match state {
        "MERGED" => PrMergeState::Merged,
        "CLOSED" => PrMergeState::Closed,
        "OPEN" if is_draft => PrMergeState::Draft,
        "OPEN" => match merge_state_status {
            // Mergeable green-path states map to Ready; anything else
            // (BLOCKED / DIRTY / UNSTABLE / UNKNOWN / None) maps to
            // plain Open so consumers see the live state rather than
            // an over-promised "Ready".
            Some("CLEAN") | Some("HAS_HOOKS") => PrMergeState::Ready,
            _ => PrMergeState::Open,
        },
        _ => PrMergeState::Open,
    }
}

fn derive_check_state(rollup: &[GhCheckRow]) -> PrCheckState {
    if rollup.is_empty() {
        // No checks configured / not yet visible — treat as Queued so
        // consumers see a definite state rather than absence. Once
        // checks land they emit a Running/Passed/Failed transition
        // event.
        return PrCheckState::Queued;
    }

    let mut any_running = false;
    let mut any_failed = false;
    let mut any_cancelled = false;
    let mut all_skipped = true;

    for row in rollup {
        let (status, conclusion, legacy_state) = (
            row.status.as_deref(),
            row.conclusion.as_deref(),
            row.state.as_deref(),
        );

        // New check-suite shape.
        match status {
            Some("QUEUED") | Some("IN_PROGRESS") => {
                any_running = true;
                all_skipped = false;
                continue;
            }
            Some("COMPLETED") => {
                match conclusion {
                    Some("FAILURE")
                    | Some("TIMED_OUT")
                    | Some("STARTUP_FAILURE")
                    | Some("ACTION_REQUIRED") => {
                        any_failed = true;
                        all_skipped = false;
                    }
                    Some("CANCELLED") => {
                        any_cancelled = true;
                        all_skipped = false;
                    }
                    Some("SKIPPED") => {}
                    Some("SUCCESS") | Some("NEUTRAL") => {
                        all_skipped = false;
                    }
                    _ => {
                        all_skipped = false;
                    }
                }
                continue;
            }
            _ => {}
        }

        // Legacy status-context shape.
        match legacy_state {
            Some("PENDING") => {
                any_running = true;
                all_skipped = false;
            }
            Some("FAILURE") | Some("ERROR") => {
                any_failed = true;
                all_skipped = false;
            }
            Some("SUCCESS") => {
                all_skipped = false;
            }
            _ => {
                all_skipped = false;
            }
        }
    }

    if any_running {
        PrCheckState::Running
    } else if any_failed {
        PrCheckState::Failed
    } else if any_cancelled {
        PrCheckState::Cancelled
    } else if all_skipped {
        PrCheckState::Skipped
    } else {
        PrCheckState::Passed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeGhRunner {
        responses: RefCell<HashMap<Vec<String>, Result<String, GhRunnerError>>>,
    }

    impl FakeGhRunner {
        fn with(args: &[&str], stdout: &str) -> Self {
            let mut map = HashMap::new();
            map.insert(
                args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                Ok(stdout.to_string()),
            );
            Self {
                responses: RefCell::new(map),
            }
        }
    }

    impl GhCommandRunner for FakeGhRunner {
        fn run_gh(&self, args: &[&str]) -> Result<String, GhRunnerError> {
            let key: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            self.responses.borrow_mut().remove(&key).unwrap_or_else(|| {
                Err(GhRunnerError::NonZero {
                    status: Some(1),
                    stderr: format!("no canned response for args: {args:?}"),
                })
            })
        }
    }

    fn list_args(repo: &str) -> Vec<&'static str> {
        // Returning &'static str keys lets the FakeGhRunner index by
        // owned Vec<String> after the test constructs them; the values
        // are static strings in the test.
        let _ = repo;
        vec![
            "pr",
            "list",
            "--repo",
            // The test sets this dynamically; this helper exists only
            // for documentation, not for direct use.
            "REPO",
            "--state",
            "all",
            "--json",
            GH_JSON_FIELDS,
        ]
    }

    #[test]
    fn list_args_documents_the_expected_invocation() {
        // Sanity: keep the documented arg list in sync with the real
        // invocation so test fixtures match what production sends.
        let args = list_args("any");
        assert_eq!(args[0], "pr");
        assert_eq!(args[1], "list");
        assert!(args.contains(&GH_JSON_FIELDS));
    }

    fn fake_with_repo(repo: &str, stdout: &str) -> FakeGhRunner {
        FakeGhRunner::with(
            &[
                "pr",
                "list",
                "--repo",
                repo,
                "--state",
                "all",
                "--json",
                GH_JSON_FIELDS,
            ],
            stdout,
        )
    }

    fn test_repo() -> RepoId {
        RepoId::new("test-org/test-repo").unwrap()
    }

    #[test]
    fn empty_list_yields_empty_snapshot() {
        let runner = fake_with_repo("test-org/test-repo", "[]");
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert!(snapshot.pulls.is_empty());
        assert_eq!(snapshot.repo, test_repo());
    }

    #[test]
    fn open_pr_with_in_progress_check_maps_to_running_open() {
        let stdout = r#"[{
            "number": 1,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "OPEN",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": [
                {"status": "IN_PROGRESS"}
            ]
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        let pr = snapshot.pulls.get(&1).expect("pr 1 present");
        assert_eq!(pr.check_state, PrCheckState::Running);
        // `CLEAN` merge-state still maps to Ready even with checks
        // running — these are independent dimensions and consumers
        // care about both.
        assert_eq!(pr.merge_state, PrMergeState::Ready);
    }

    #[test]
    fn open_pr_with_all_success_maps_to_passed() {
        let stdout = r#"[{
            "number": 2,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "OPEN",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": [
                {"status": "COMPLETED", "conclusion": "SUCCESS"},
                {"status": "COMPLETED", "conclusion": "SUCCESS"}
            ]
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&2).unwrap().check_state,
            PrCheckState::Passed
        );
    }

    #[test]
    fn any_failure_maps_to_failed_even_with_success_siblings() {
        let stdout = r#"[{
            "number": 3,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "OPEN",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": [
                {"status": "COMPLETED", "conclusion": "SUCCESS"},
                {"status": "COMPLETED", "conclusion": "FAILURE"}
            ]
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&3).unwrap().check_state,
            PrCheckState::Failed
        );
    }

    #[test]
    fn empty_rollup_maps_to_queued() {
        let stdout = r#"[{
            "number": 4,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "OPEN",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": []
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&4).unwrap().check_state,
            PrCheckState::Queued
        );
    }

    #[test]
    fn draft_pr_maps_to_draft_merge_state() {
        let stdout = r#"[{
            "number": 5,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "OPEN",
            "isDraft": true,
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": []
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&5).unwrap().merge_state,
            PrMergeState::Draft
        );
    }

    #[test]
    fn merged_pr_maps_to_merged() {
        let stdout = r#"[{
            "number": 6,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "MERGED",
            "isDraft": false,
            "statusCheckRollup": [
                {"status": "COMPLETED", "conclusion": "SUCCESS"}
            ]
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&6).unwrap().merge_state,
            PrMergeState::Merged
        );
    }

    #[test]
    fn closed_unmerged_pr_maps_to_closed() {
        let stdout = r#"[{
            "number": 7,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "CLOSED",
            "isDraft": false,
            "statusCheckRollup": []
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&7).unwrap().merge_state,
            PrMergeState::Closed
        );
    }

    #[test]
    fn blocked_merge_state_stays_open_not_ready() {
        let stdout = r#"[{
            "number": 8,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "OPEN",
            "isDraft": false,
            "mergeStateStatus": "BLOCKED",
            "statusCheckRollup": [
                {"status": "COMPLETED", "conclusion": "SUCCESS"}
            ]
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&8).unwrap().merge_state,
            PrMergeState::Open,
            "BLOCKED must not be advertised as Ready"
        );
    }

    #[test]
    fn legacy_pending_state_maps_to_running() {
        let stdout = r#"[{
            "number": 9,
            "headRefName": "test-head",
            "baseRefName": "test-base",
            "state": "OPEN",
            "isDraft": false,
            "mergeStateStatus": "CLEAN",
            "statusCheckRollup": [
                {"state": "PENDING"}
            ]
        }]"#;
        let runner = fake_with_repo("test-org/test-repo", stdout);
        let source = GhPullRequestSource::new(runner);
        let snapshot = source.snapshot(&test_repo()).expect("snapshot");
        assert_eq!(
            snapshot.pulls.get(&9).unwrap().check_state,
            PrCheckState::Running
        );
    }

    #[test]
    fn gh_failure_surfaces_as_source_error() {
        let runner = FakeGhRunner::default();
        let source = GhPullRequestSource::new(runner);
        let result = source.snapshot(&test_repo());
        assert!(matches!(result, Err(PullRequestSourceError::Source(_))));
    }

    #[test]
    fn malformed_json_surfaces_as_source_error() {
        let runner = fake_with_repo("test-org/test-repo", "not json");
        let source = GhPullRequestSource::new(runner);
        let result = source.snapshot(&test_repo());
        assert!(matches!(result, Err(PullRequestSourceError::Source(_))));
    }
}
