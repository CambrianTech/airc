//! Append-only work-domain events.

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};
use crate::model::{
    BranchName, CardState, DrainCandidate, DrainOutcome, HygieneReport, LaneState, PressureLevel,
    Priority, PullRequestRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkEvent {
    CardCreated(CardCreated),
    CardClaimed(WorkCardClaimed),
    ClaimHeartbeat(ClaimHeartbeat),
    ClaimReleased(ClaimReleased),
    CardStateChanged(CardStateChanged),
    LaneCreated(LaneCreated),
    LaneStateChanged(LaneStateChanged),
    WorkspaceRequested(WorkspaceRequested),
    WorkspaceAllocated(WorkspaceAllocated),
    WorkspaceHeartbeat(WorkspaceHeartbeat),
    WorkspaceReleased(WorkspaceReleased),
    WorkspacePressureReported(WorkspacePressureReported),
    WorkspaceDrainRequested(WorkspaceDrainRequested),
    WorkspaceDrainCompleted(WorkspaceDrainCompleted),
    PullRequestLinked(PullRequestLinked),
    PullRequestMerged(PullRequestMerged),
    HygieneReportRecorded(HygieneReportRecorded),
    ManagerHatClaimed(ManagerHatClaimed),
    ManagerHatReleased(ManagerHatReleased),
}

impl WorkEvent {
    pub fn occurred_at_ms(&self) -> u64 {
        match self {
            WorkEvent::CardCreated(e) => e.created_at_ms,
            WorkEvent::CardClaimed(e) => e.claimed_at_ms,
            WorkEvent::ClaimHeartbeat(e) => e.heartbeat_at_ms,
            WorkEvent::ClaimReleased(e) => e.released_at_ms,
            WorkEvent::CardStateChanged(e) => e.changed_at_ms,
            WorkEvent::LaneCreated(e) => e.created_at_ms,
            WorkEvent::LaneStateChanged(e) => e.changed_at_ms,
            WorkEvent::WorkspaceRequested(e) => e.requested_at_ms,
            WorkEvent::WorkspaceAllocated(e) => e.allocated_at_ms,
            WorkEvent::WorkspaceHeartbeat(e) => e.heartbeat_at_ms,
            WorkEvent::WorkspaceReleased(e) => e.released_at_ms,
            WorkEvent::WorkspacePressureReported(e) => e.reported_at_ms,
            WorkEvent::WorkspaceDrainRequested(e) => e.requested_at_ms,
            WorkEvent::WorkspaceDrainCompleted(e) => e.completed_at_ms,
            WorkEvent::PullRequestLinked(e) => e.linked_at_ms,
            WorkEvent::PullRequestMerged(e) => e.merged_at_ms,
            WorkEvent::HygieneReportRecorded(e) => e.report.recorded_at_ms,
            WorkEvent::ManagerHatClaimed(e) => e.claimed_at_ms,
            WorkEvent::ManagerHatReleased(e) => e.released_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardCreated {
    pub card_id: WorkCardId,
    pub repo: RepoId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub lane_id: Option<LaneId>,
    pub created_by: PeerId,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCardClaimed {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub ttl_ms: u64,
    pub claimed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimHeartbeat {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub ttl_ms: u64,
    pub heartbeat_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReleased {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub reason: Option<String>,
    pub released_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardStateChanged {
    pub card_id: WorkCardId,
    pub state: CardState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCreated {
    pub lane_id: LaneId,
    pub repo: RepoId,
    pub title: String,
    pub state: LaneState,
    pub created_by: PeerId,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStateChanged {
    pub lane_id: LaneId,
    pub state: LaneState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRequested {
    pub workspace_id: WorkspaceId,
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub repo: RepoId,
    pub branch: BranchName,
    pub base: BranchName,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAllocated {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub allocated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHeartbeat {
    pub workspace_id: WorkspaceId,
    pub disk_bytes: Option<u64>,
    pub heartbeat_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReleased {
    pub workspace_id: WorkspaceId,
    pub released_at_ms: u64,
}

/// Disk pressure observation for a workspace. Telemetry event; the
/// emitter is whichever peer has visibility into the workspace's disk
/// state. Workspace-id keyed and intentionally independent of the
/// card/claim lease flow — pressure can be observed and reported on
/// any known `WorkspaceId`, leased or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePressureReported {
    pub workspace_id: WorkspaceId,
    pub repo: RepoId,
    pub reporter: PeerId,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub level: PressureLevel,
    pub reported_at_ms: u64,
}

/// Drain request. Captures the candidate list at decision time so the
/// completion outcome can be compared against intent in record/replay.
/// Workspace-id keyed; no card/claim coupling required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDrainRequested {
    pub workspace_id: WorkspaceId,
    pub repo: RepoId,
    pub requester: PeerId,
    /// Stable identifier of the policy rule that emitted this request
    /// (e.g. `"default.rebuildable"`, `"user.aggressive"`). Lets the
    /// runtime correlate outcomes to rules over time.
    pub policy_rule_id: String,
    /// True = inspection only, no paths are touched. Completion must
    /// echo `dry_run = true` and `paths_touched` must be empty.
    pub dry_run: bool,
    pub candidates: Vec<DrainCandidate>,
    pub requested_at_ms: u64,
}

/// Drain completion. Honest about partial outcomes — see [`DrainOutcome`]
/// for the bytes/paths/errors fields. `performer` records which peer
/// actually executed the cleanup so audit can attribute reclaim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDrainCompleted {
    pub workspace_id: WorkspaceId,
    pub repo: RepoId,
    pub performer: PeerId,
    pub policy_rule_id: String,
    pub dry_run: bool,
    pub outcome: DrainOutcome,
    pub completed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestLinked {
    pub card_id: WorkCardId,
    pub pull_request: PullRequestRef,
    pub linked_by: PeerId,
    pub linked_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestMerged {
    pub card_id: WorkCardId,
    pub pull_request: PullRequestRef,
    pub merged_by: PeerId,
    pub merged_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HygieneReportRecorded {
    pub report: HygieneReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerHatClaimed {
    pub repo: RepoId,
    pub manager: PeerId,
    pub ttl_ms: u64,
    pub claimed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerHatReleased {
    pub repo: RepoId,
    pub manager: PeerId,
    pub released_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_event_serializes_with_kind_tag() {
        let event = WorkEvent::CardCreated(CardCreated {
            card_id: WorkCardId::from_u128(1),
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "build work domain".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: PeerId::from_u128(2),
            created_at_ms: 10,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "card_created");
        assert_eq!(event.occurred_at_ms(), 10);
    }
}
