//! Pull-request observation adapter.
//!
//! Mirrors [`crate::local_git`] shape: a [`PullRequestSource`] trait
//! abstracts the I/O surface (real impls shell out to `gh` or hit the
//! REST API), [`PullRequestObserver`] produces a typed snapshot, and
//! [`pull_request_events_since`] diffs two snapshots and emits the PR
//! events already defined in [`crate::event`]
//! (`PullRequestCheckSuiteChanged`, `PullRequestMergeStateChanged`,
//! `PullRequestReviewSubmitted`).
//!
//! This is the **skeleton** — the trait, snapshot type, diff logic,
//! and a stub in-memory source for tests. A real `gh`-CLI source is
//! intentionally out of scope here; it ships in a follow-up so this
//! lands as a clean substrate-level contract that consumers
//! (Continuum, OpenClaw, Hermes, agents) can subscribe against
//! immediately.

use std::collections::HashMap;

use airc_core::PeerId;
use serde::{Deserialize, Serialize};

use crate::event::{
    PullRequestCheckSuiteChanged, PullRequestMergeStateChanged, PullRequestReviewSubmitted,
    WorkEvent,
};
use crate::model::{PrCheckState, PrMergeState, PrReviewState, PullRequestRef};
use crate::RepoId;

/// One PR's observed state at a point in time.
///
/// Reviews are keyed by reviewer `PeerId`. Each reviewer carries one
/// terminal review state — the diff treats a reviewer's first
/// observation as a submission and any subsequent state change as a
/// new submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSnapshot {
    pub pull_request: PullRequestRef,
    pub check_state: PrCheckState,
    pub merge_state: PrMergeState,
    pub reviews: HashMap<PeerId, PrReviewState>,
}

/// All PRs the adapter saw on one repo on a single observation. Keyed
/// by PR number so [`pull_request_events_since`] can pair previous
/// and current rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPullRequestSnapshot {
    pub repo: RepoId,
    pub pulls: HashMap<u64, PullRequestSnapshot>,
}

/// I/O abstraction for "read the current state of all open PRs on
/// `repo`." Real impls call `gh` / REST and translate. Stubs return
/// canned snapshots for tests.
pub trait PullRequestSource {
    fn snapshot(&self, repo: &RepoId) -> Result<RepoPullRequestSnapshot, PullRequestSourceError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PullRequestSourceError {
    #[error("pull request source: {0}")]
    Source(String),
}

/// Stateless observer paired with a source. Callers persist the
/// returned snapshot between calls and feed it back as `previous` —
/// same contract as [`crate::local_git::LocalGitObserver`].
#[derive(Debug, Clone)]
pub struct PullRequestObserver<S> {
    source: S,
}

impl<S: PullRequestSource> PullRequestObserver<S> {
    pub fn new(source: S) -> Self {
        Self { source }
    }

    pub fn observe(
        &self,
        repo: &RepoId,
    ) -> Result<RepoPullRequestSnapshot, PullRequestSourceError> {
        self.source.snapshot(repo)
    }
}

/// Diff two snapshots and emit one event per state change. Same
/// contract as [`crate::local_git::local_git_events_since`]:
///
/// - When `previous` is `None` (cold start) every observed PR
///   produces a check-state + merge-state event so consumers see the
///   initial state from cursor replay.
/// - For each PR present in both, an event is emitted only when the
///   state actually changed.
/// - Reviews are diffed per reviewer; a new reviewer or a state
///   change emits one `PullRequestReviewSubmitted` per affected
///   reviewer.
/// - PRs that disappear from `current` produce no events — consumers
///   distinguish "closed" from "no longer in the open-PR window" via
///   `PrMergeState::Merged`/`Closed`, not via absence.
pub fn pull_request_events_since(
    previous: Option<&RepoPullRequestSnapshot>,
    current: &RepoPullRequestSnapshot,
    observed_by: PeerId,
    observed_at_ms: u64,
) -> Vec<WorkEvent> {
    let mut events = Vec::new();
    let empty: HashMap<u64, PullRequestSnapshot> = HashMap::new();
    let prev_pulls = previous.map(|p| &p.pulls).unwrap_or(&empty);

    // Deterministic iteration so the emitted event order is stable
    // across runs even though HashMap iteration is not.
    let mut numbers: Vec<u64> = current.pulls.keys().copied().collect();
    numbers.sort_unstable();

    for number in numbers {
        let current_pr = &current.pulls[&number];
        let previous_pr = prev_pulls.get(&number);

        if previous_pr.is_none_or(|p| p.check_state != current_pr.check_state) {
            events.push(WorkEvent::PullRequestCheckSuiteChanged(
                PullRequestCheckSuiteChanged {
                    pull_request: current_pr.pull_request.clone(),
                    state: current_pr.check_state,
                    changed_by: observed_by,
                    changed_at_ms: observed_at_ms,
                },
            ));
        }

        if previous_pr.is_none_or(|p| p.merge_state != current_pr.merge_state) {
            events.push(WorkEvent::PullRequestMergeStateChanged(
                PullRequestMergeStateChanged {
                    pull_request: current_pr.pull_request.clone(),
                    state: current_pr.merge_state,
                    changed_by: observed_by,
                    changed_at_ms: observed_at_ms,
                },
            ));
        }

        let mut reviewers: Vec<PeerId> = current_pr.reviews.keys().copied().collect();
        reviewers.sort_by_key(|peer| peer.to_string());
        for reviewer in reviewers {
            let current_review = current_pr.reviews[&reviewer];
            let prev_review = previous_pr.and_then(|p| p.reviews.get(&reviewer)).copied();
            if prev_review != Some(current_review) {
                events.push(WorkEvent::PullRequestReviewSubmitted(
                    PullRequestReviewSubmitted {
                        pull_request: current_pr.pull_request.clone(),
                        reviewer,
                        state: current_review,
                        submitted_at_ms: observed_at_ms,
                    },
                ));
            }
        }
    }

    events
}

/// Test/stub source. Holds a canned snapshot per repo and returns it
/// verbatim on `snapshot()`. Useful for integration tests that need
/// to drive the lib-side `observe_pull_requests` API without
/// reaching for `gh`.
#[derive(Debug, Clone, Default)]
pub struct InMemoryPullRequestSource {
    snapshots: HashMap<RepoId, RepoPullRequestSnapshot>,
}

impl InMemoryPullRequestSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_snapshot(mut self, snapshot: RepoPullRequestSnapshot) -> Self {
        self.snapshots.insert(snapshot.repo.clone(), snapshot);
        self
    }

    pub fn set(&mut self, snapshot: RepoPullRequestSnapshot) {
        self.snapshots.insert(snapshot.repo.clone(), snapshot);
    }
}

impl PullRequestSource for InMemoryPullRequestSource {
    fn snapshot(&self, repo: &RepoId) -> Result<RepoPullRequestSnapshot, PullRequestSourceError> {
        match self.snapshots.get(repo) {
            Some(snapshot) => Ok(snapshot.clone()),
            None => Ok(RepoPullRequestSnapshot {
                repo: repo.clone(),
                pulls: HashMap::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BranchName;

    fn repo() -> RepoId {
        RepoId::new("test-org/test-repo").unwrap()
    }

    fn pr_ref(number: u64) -> PullRequestRef {
        PullRequestRef {
            repo: repo(),
            number,
            head: BranchName::new("test-head").unwrap(),
            base: BranchName::new("test-base").unwrap(),
        }
    }

    fn pr_snapshot(
        number: u64,
        check: PrCheckState,
        merge: PrMergeState,
        reviews: &[(PeerId, PrReviewState)],
    ) -> PullRequestSnapshot {
        PullRequestSnapshot {
            pull_request: pr_ref(number),
            check_state: check,
            merge_state: merge,
            reviews: reviews.iter().copied().collect(),
        }
    }

    fn repo_snapshot(prs: Vec<PullRequestSnapshot>) -> RepoPullRequestSnapshot {
        let pulls = prs
            .into_iter()
            .map(|pr| (pr.pull_request.number, pr))
            .collect();
        RepoPullRequestSnapshot {
            repo: repo(),
            pulls,
        }
    }

    #[test]
    fn cold_start_emits_check_and_merge_for_every_pr() {
        let observer = PeerId::new();
        let current = repo_snapshot(vec![
            pr_snapshot(1, PrCheckState::Running, PrMergeState::Open, &[]),
            pr_snapshot(2, PrCheckState::Passed, PrMergeState::Ready, &[]),
        ]);
        let events = pull_request_events_since(None, &current, observer, 1_000);
        // Two PRs × (check + merge) = 4 events, no review events.
        assert_eq!(events.len(), 4);
        let kinds: Vec<&'static str> = events
            .iter()
            .map(|e| match e {
                WorkEvent::PullRequestCheckSuiteChanged(_) => "check",
                WorkEvent::PullRequestMergeStateChanged(_) => "merge",
                WorkEvent::PullRequestReviewSubmitted(_) => "review",
                _ => "other",
            })
            .collect();
        // Deterministic: PR 901 (check, merge), PR 902 (check, merge).
        assert_eq!(kinds, vec!["check", "merge", "check", "merge"]);
    }

    #[test]
    fn unchanged_pr_emits_nothing() {
        let snapshot = repo_snapshot(vec![pr_snapshot(
            3,
            PrCheckState::Passed,
            PrMergeState::Ready,
            &[],
        )]);
        let events = pull_request_events_since(Some(&snapshot), &snapshot, PeerId::new(), 2_000);
        assert!(events.is_empty(), "no diff → no events; got {events:?}");
    }

    #[test]
    fn check_state_change_emits_one_check_event() {
        let prev = repo_snapshot(vec![pr_snapshot(
            4,
            PrCheckState::Running,
            PrMergeState::Open,
            &[],
        )]);
        let current = repo_snapshot(vec![pr_snapshot(
            4,
            PrCheckState::Passed,
            PrMergeState::Open,
            &[],
        )]);
        let events = pull_request_events_since(Some(&prev), &current, PeerId::new(), 3_000);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkEvent::PullRequestCheckSuiteChanged(payload) => {
                assert_eq!(payload.state, PrCheckState::Passed);
                assert_eq!(payload.pull_request.number, 4);
            }
            other => panic!("expected check-state event, got {other:?}"),
        }
    }

    #[test]
    fn new_reviewer_emits_review_event() {
        let reviewer = PeerId::new();
        let prev = repo_snapshot(vec![pr_snapshot(
            5,
            PrCheckState::Running,
            PrMergeState::Open,
            &[],
        )]);
        let current = repo_snapshot(vec![pr_snapshot(
            5,
            PrCheckState::Running,
            PrMergeState::Open,
            &[(reviewer, PrReviewState::Approved)],
        )]);
        let events = pull_request_events_since(Some(&prev), &current, PeerId::new(), 4_000);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkEvent::PullRequestReviewSubmitted(payload) => {
                assert_eq!(payload.reviewer, reviewer);
                assert_eq!(payload.state, PrReviewState::Approved);
            }
            other => panic!("expected review event, got {other:?}"),
        }
    }

    #[test]
    fn reviewer_state_change_emits_one_review_event() {
        let reviewer = PeerId::new();
        let prev = repo_snapshot(vec![pr_snapshot(
            6,
            PrCheckState::Running,
            PrMergeState::Open,
            &[(reviewer, PrReviewState::Requested)],
        )]);
        let current = repo_snapshot(vec![pr_snapshot(
            6,
            PrCheckState::Running,
            PrMergeState::Open,
            &[(reviewer, PrReviewState::Approved)],
        )]);
        let events = pull_request_events_since(Some(&prev), &current, PeerId::new(), 5_000);
        assert_eq!(events.len(), 1);
        match &events[0] {
            WorkEvent::PullRequestReviewSubmitted(payload) => {
                assert_eq!(payload.state, PrReviewState::Approved);
            }
            other => panic!("expected review event, got {other:?}"),
        }
    }

    #[test]
    fn dropped_pr_emits_no_event() {
        // A PR present in `prev` but absent from `current` is treated
        // as out-of-window, not as a transition. Closed/merged
        // transitions are conveyed via `PrMergeState`, not by
        // dropping the row.
        let prev = repo_snapshot(vec![pr_snapshot(
            7,
            PrCheckState::Passed,
            PrMergeState::Open,
            &[],
        )]);
        let current = repo_snapshot(vec![]);
        let events = pull_request_events_since(Some(&prev), &current, PeerId::new(), 6_000);
        assert!(
            events.is_empty(),
            "dropped PR must not emit events; got {events:?}"
        );
    }

    #[test]
    fn in_memory_source_returns_canned_snapshot() {
        let canned = repo_snapshot(vec![pr_snapshot(
            8,
            PrCheckState::Queued,
            PrMergeState::Draft,
            &[],
        )]);
        let source = InMemoryPullRequestSource::new().with_snapshot(canned.clone());
        let observer = PullRequestObserver::new(source);
        let observed = observer.observe(&repo()).expect("snapshot");
        assert_eq!(observed, canned);
    }

    #[test]
    fn in_memory_source_returns_empty_for_unknown_repo() {
        let other = RepoId::new("CambrianTech/other").unwrap();
        let source = InMemoryPullRequestSource::new();
        let observer = PullRequestObserver::new(source);
        let observed = observer.observe(&other).expect("snapshot");
        assert!(observed.pulls.is_empty());
        assert_eq!(observed.repo, other);
    }
}
