//! In-memory projections built by replaying [`WorkEvent`]s.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::event::{
    GitCommitObserved, GitDirtyStateChanged, PullRequestCheckSuiteChanged,
    PullRequestMergeStateChanged, PullRequestReviewSubmitted, WorkspaceDrainCompleted,
    WorkspaceDrainRequested, WorkspacePressureReported,
};
use crate::ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};
use crate::model::{
    BranchName, GitObjectId, HygieneReport, LaneState, PrCheckState, PrMergeState, PrReviewState,
    PullRequestRef, WorkCard, WorkspaceLease,
};

mod apply;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkBoardProjection {
    pub(super) cards: BTreeMap<WorkCardId, WorkCard>,
    pub(super) lanes: BTreeMap<LaneId, LaneRecord>,
    pub(super) workspaces: BTreeMap<WorkspaceId, WorkspaceRecord>,
    /// Latest pressure observation per workspace. Independent of
    /// [`WorkspaceRecord`] — pressure is keyed by `WorkspaceId`
    /// regardless of lease state, so consumers without leases can
    /// participate in hygiene.
    pub(super) workspace_pressure: BTreeMap<WorkspaceId, WorkspacePressureReported>,
    /// In-flight drain requests, keyed by `(workspace_id, policy_rule_id)`.
    /// Removed when the matching `WorkspaceDrainCompleted` lands. Same
    /// rule cannot have two concurrent requests on one workspace; if it
    /// does, the latter request replaces the former — that's a policy
    /// bug the projection surfaces by overwriting, not by erroring.
    pub(super) pending_drains: BTreeMap<(WorkspaceId, String), WorkspaceDrainRequested>,
    /// Append-only history of completed drains across all workspaces.
    /// Consumers paginate / filter by `workspace_id` via accessors.
    pub(super) drain_history: Vec<WorkspaceDrainCompleted>,
    pub(super) repo_tracking: BTreeMap<RepoId, RepoTrackingRecord>,
    pub(super) pull_requests: BTreeMap<String, PullRequestRecord>,
    pub(super) manager_hats: BTreeMap<RepoId, ManagerHat>,
    pub(super) hygiene_reports: Vec<HygieneReport>,
}

impl WorkBoardProjection {
    /// Latest pressure observation for a workspace, if any reporter has
    /// emitted one.
    pub fn workspace_pressure(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Option<&WorkspacePressureReported> {
        self.workspace_pressure.get(workspace_id)
    }

    /// Drain requests awaiting completion for a workspace.
    pub fn pending_drains_for(&self, workspace_id: &WorkspaceId) -> Vec<&WorkspaceDrainRequested> {
        self.pending_drains
            .iter()
            .filter(|((ws, _), _)| ws == workspace_id)
            .map(|(_, request)| request)
            .collect()
    }

    /// Completed drains for a workspace, in event-stream order.
    pub fn drain_history_for(&self, workspace_id: &WorkspaceId) -> Vec<&WorkspaceDrainCompleted> {
        self.drain_history
            .iter()
            .filter(|d| &d.workspace_id == workspace_id)
            .collect()
    }

    pub fn repo_tracking(&self, repo: &RepoId) -> Option<&RepoTrackingRecord> {
        self.repo_tracking.get(repo)
    }

    pub fn pull_request(&self, repo: &RepoId, number: u64) -> Option<&PullRequestRecord> {
        self.pull_requests.get(&pull_request_key(repo, number))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub cards: Vec<WorkCard>,
    pub lanes: Vec<LaneRecord>,
    pub workspaces: Vec<WorkspaceRecord>,
    pub repo_tracking: Vec<RepoTrackingRecord>,
    pub pull_requests: Vec<PullRequestRecord>,
    pub manager_hats: Vec<ManagerHat>,
    pub hygiene_reports: Vec<HygieneReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRecord {
    pub lane_id: LaneId,
    pub repo: RepoId,
    pub title: String,
    pub state: LaneState,
    pub card_ids: Vec<WorkCardId>,
    pub created_by: PeerId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub lease: WorkspaceLease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoTrackingRecord {
    pub repo: RepoId,
    pub branches: BTreeMap<BranchName, BranchTrackingRecord>,
    pub observed_commits: Vec<GitCommitObserved>,
    pub dirty_states: Vec<GitDirtyStateChanged>,
}

impl RepoTrackingRecord {
    fn new(repo: RepoId) -> Self {
        Self {
            repo,
            branches: BTreeMap::new(),
            observed_commits: Vec::new(),
            dirty_states: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchTrackingRecord {
    pub branch: BranchName,
    pub head: GitObjectId,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRecord {
    pub pull_request: PullRequestRef,
    pub check_state: Option<PrCheckState>,
    pub review_state: Option<PrReviewState>,
    pub merge_state: Option<PrMergeState>,
    pub updated_at_ms: u64,
}

impl PullRequestRecord {
    fn from_check(event: &PullRequestCheckSuiteChanged) -> Self {
        Self {
            pull_request: event.pull_request.clone(),
            check_state: Some(event.state),
            review_state: None,
            merge_state: None,
            updated_at_ms: event.changed_at_ms,
        }
    }

    fn from_review(event: &PullRequestReviewSubmitted) -> Self {
        Self {
            pull_request: event.pull_request.clone(),
            check_state: None,
            review_state: Some(event.state),
            merge_state: None,
            updated_at_ms: event.submitted_at_ms,
        }
    }

    fn from_merge(event: &PullRequestMergeStateChanged) -> Self {
        Self {
            pull_request: event.pull_request.clone(),
            check_state: None,
            review_state: None,
            merge_state: Some(event.state),
            updated_at_ms: event.changed_at_ms,
        }
    }

    fn apply_check(&mut self, event: &PullRequestCheckSuiteChanged) {
        self.pull_request = event.pull_request.clone();
        self.check_state = Some(event.state);
        self.updated_at_ms = event.changed_at_ms;
    }

    fn apply_review(&mut self, event: &PullRequestReviewSubmitted) {
        self.pull_request = event.pull_request.clone();
        self.review_state = Some(event.state);
        self.updated_at_ms = event.submitted_at_ms;
    }

    fn apply_merge(&mut self, event: &PullRequestMergeStateChanged) {
        self.pull_request = event.pull_request.clone();
        self.merge_state = Some(event.state);
        self.updated_at_ms = event.changed_at_ms;
    }
}

pub(super) fn pull_request_key(repo: &RepoId, number: u64) -> String {
    format!("{repo}#{number}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerHat {
    pub repo: RepoId,
    pub manager: PeerId,
    pub expires_at_ms: u64,
    pub claimed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleClaim {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub expired_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    #[error("duplicate card {0}")]
    DuplicateCard(WorkCardId),
    #[error("unknown card {0}")]
    UnknownCard(WorkCardId),
    #[error("duplicate lane {0}")]
    DuplicateLane(LaneId),
    #[error("unknown lane {0}")]
    UnknownLane(LaneId),
    #[error("unknown workspace {0}")]
    UnknownWorkspace(WorkspaceId),
    #[error("claim mismatch for card {card_id}: expected {expected:?}, got {got}")]
    ClaimMismatch {
        card_id: WorkCardId,
        expected: Option<ClaimId>,
        got: ClaimId,
    },
}

#[cfg(test)]
mod tests;
