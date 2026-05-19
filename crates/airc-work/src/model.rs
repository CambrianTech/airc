//! Stable work-domain data models.

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    P0,
    P1,
    #[default]
    P2,
    P3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Open,
    Claimed,
    InProgress,
    Blocked,
    Review,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    Planned,
    Active,
    Blocked,
    Landing,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Requested,
    Allocated,
    Active,
    Released,
    Orphaned,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BranchName(String);

impl BranchName {
    pub fn new(value: impl Into<String>) -> Result<Self, BranchNameError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(BranchNameError::Empty);
        }
        if trimmed.contains(char::is_whitespace) {
            return Err(BranchNameError::ContainsWhitespace);
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BranchName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BranchNameError {
    #[error("branch name cannot be empty")]
    Empty,
    #[error("branch name cannot contain whitespace")]
    ContainsWhitespace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRef {
    pub repo: RepoId,
    pub number: u64,
    pub head: BranchName,
    pub base: BranchName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCard {
    pub card_id: WorkCardId,
    pub repo: RepoId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub lane_id: Option<LaneId>,
    pub state: CardState,
    pub owner: Option<PeerId>,
    pub claim_id: Option<ClaimId>,
    pub claim_expires_at_ms: Option<u64>,
    pub last_heartbeat_at_ms: Option<u64>,
    pub pull_request: Option<PullRequestRef>,
    pub created_by: PeerId,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLease {
    pub workspace_id: WorkspaceId,
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub repo: RepoId,
    pub path: String,
    pub branch: BranchName,
    pub base: BranchName,
    pub status: WorkspaceStatus,
    pub disk_bytes: Option<u64>,
    pub created_at_ms: u64,
    pub heartbeat_at_ms: u64,
    pub released_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HygieneReport {
    pub repo: RepoId,
    pub reporter: PeerId,
    pub total_bytes: u64,
    pub rebuildable_bytes: u64,
    pub workspace_count: usize,
    pub recorded_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_name_rejects_empty_and_whitespace() {
        assert!(matches!(BranchName::new(""), Err(BranchNameError::Empty)));
        assert!(matches!(
            BranchName::new("feat/bad branch"),
            Err(BranchNameError::ContainsWhitespace)
        ));
        assert_eq!(
            BranchName::new(" feat/good ").unwrap().as_str(),
            "feat/good"
        );
    }
}
