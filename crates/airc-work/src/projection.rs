//! In-memory projections built by replaying [`WorkEvent`]s.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};
use crate::model::{HygieneReport, LaneState, WorkCard, WorkspaceLease};

mod apply;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkBoardProjection {
    pub(super) cards: BTreeMap<WorkCardId, WorkCard>,
    pub(super) lanes: BTreeMap<LaneId, LaneRecord>,
    pub(super) workspaces: BTreeMap<WorkspaceId, WorkspaceRecord>,
    pub(super) manager_hats: BTreeMap<RepoId, ManagerHat>,
    pub(super) hygiene_reports: Vec<HygieneReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub cards: Vec<WorkCard>,
    pub lanes: Vec<LaneRecord>,
    pub workspaces: Vec<WorkspaceRecord>,
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
