//! Append-only work-domain events.

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::goal_event::{
    CardOrigin, GoalAbandoned, GoalAchieved, GoalCreated, GoalDryTickRecorded,
};
use crate::ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};
use crate::model::{
    AgentAvailabilityState, BranchName, CardState, DirtyState, DrainCandidate, DrainOutcome,
    GitObjectId, HygieneReport, LaneState, PrCheckState, PrMergeState, PrReviewState,
    PressureLevel, Priority, PullRequestRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkEvent {
    CardCreated(CardCreated),
    CardUpdated(CardUpdated),
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
    GitCommitObserved(GitCommitObserved),
    GitBranchMoved(GitBranchMoved),
    GitDirtyStateChanged(GitDirtyStateChanged),
    PullRequestCheckSuiteChanged(PullRequestCheckSuiteChanged),
    PullRequestReviewSubmitted(PullRequestReviewSubmitted),
    PullRequestMergeStateChanged(PullRequestMergeStateChanged),
    PullRequestLinked(PullRequestLinked),
    PullRequestMerged(PullRequestMerged),
    HygieneReportRecorded(HygieneReportRecorded),
    ManagerHatClaimed(ManagerHatClaimed),
    ManagerHatReleased(ManagerHatReleased),
    AgentAvailabilityReported(AgentAvailabilityReported),
    // Card e4cad280 slice C2a: idle-agent engine goal-lifecycle events.
    // Goal creation, operator-path achievement, and abandonment ride
    // the WorkEvent union so the codec, replay, and subscription paths
    // all carry them without per-event-type plumbing. Per the verdict
    // residual 4 on PR #1123 ('every replayer re-derives the condition;
    // if each emits, the log gets N copies'), the auto-projection path
    // for ExitCondition::{DryForTicks/MilestoneClosed/AllCardsClosed}
    // is PURELY DERIVED state — no event on the wire. GoalAchieved
    // exists only for the operator path. GoalDryTickRecorded is the
    // synthesizer's typed dry-tick witness (v2 residual 1 fix).
    GoalCreated(GoalCreated),
    GoalAchieved(GoalAchieved),
    GoalAbandoned(GoalAbandoned),
    GoalDryTickRecorded(GoalDryTickRecorded),
}

impl WorkEvent {
    pub fn occurred_at_ms(&self) -> u64 {
        match self {
            WorkEvent::CardCreated(e) => e.created_at_ms,
            WorkEvent::CardUpdated(e) => e.updated_at_ms,
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
            WorkEvent::GitCommitObserved(e) => e.observed_at_ms,
            WorkEvent::GitBranchMoved(e) => e.moved_at_ms,
            WorkEvent::GitDirtyStateChanged(e) => e.changed_at_ms,
            WorkEvent::PullRequestCheckSuiteChanged(e) => e.changed_at_ms,
            WorkEvent::PullRequestReviewSubmitted(e) => e.submitted_at_ms,
            WorkEvent::PullRequestMergeStateChanged(e) => e.changed_at_ms,
            WorkEvent::PullRequestLinked(e) => e.linked_at_ms,
            WorkEvent::PullRequestMerged(e) => e.merged_at_ms,
            WorkEvent::HygieneReportRecorded(e) => e.report.recorded_at_ms,
            WorkEvent::ManagerHatClaimed(e) => e.claimed_at_ms,
            WorkEvent::ManagerHatReleased(e) => e.released_at_ms,
            WorkEvent::AgentAvailabilityReported(e) => e.reported_at_ms,
            WorkEvent::GoalCreated(e) => e.created_at_ms,
            WorkEvent::GoalAchieved(e) => e.achieved_at_ms,
            WorkEvent::GoalAbandoned(e) => e.abandoned_at_ms,
            WorkEvent::GoalDryTickRecorded(e) => e.recorded_at_ms,
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
    /// If this card is a sibling review of another card, the
    /// reviewed card's id. Card ad7e100b (peer-agent review loop)
    /// Sub-A: makes "this card is a review of X" a typed link
    /// rather than a body-string convention, so observers /
    /// scheduling logic can ask the projection
    /// (`WorkBoardProjection::review_cards_for(parent_id)`) which
    /// reviews exist for a card.
    ///
    /// Optional: regular cards omit it. Serde-back-compat: legacy
    /// `CardCreated` events on the wire decode with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews: Option<WorkCardId>,
    /// Typed provenance per v2 A1 (PR #1123 verdict). Card e4cad280
    /// slice C2a: the `Synthesized` variant carries dedup_key, goal_id,
    /// recipe_id, and synthesizer_peer so the projection (C2b) can
    /// arbitrate first-write-wins and attribute the audit trail without
    /// joining a side-channel event. Per
    /// `[[strong-typing-across-boundaries]]` and the positron #1602
    /// "first-class but never anonymous" precedent: provenance rides
    /// the primary object.
    ///
    /// Optional with `#[serde(default)]` so legacy `CardCreated` events
    /// (every card filed before C2a lands) decode with `None`. The
    /// projection treats `None` as the `Operator { peer_id: created_by }`
    /// origin per the legacy-event interpretation rule (no `Synthesized`
    /// origin can be inferred without explicit metadata — silent
    /// inference would violate `[[no-fallbacks-ever]]`). Same back-compat
    /// shape as the `reviews` field on this struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<CardOrigin>,
}

/// Amend a card's editable fields after creation. Card 5ac0a359 —
/// recurring friction during the autonomous flywheel: cards are
/// often filed with provisional bodies that need updating once
/// decomposition or scope refinement happens, and the substrate
/// previously had no typed way to do that without close+re-create
/// (which loses the card id, breaks `reviews` links pointing at the
/// old id, and forces every observer to re-project).
///
/// Each updatable field is `Option`; `None` means "leave the
/// projection's current value alone." An event with every field
/// `None` is legal and projects as a no-op (updating only
/// `updated_at_ms`) — convenient for liveness markers / "I touched
/// this card" without changing semantics.
///
/// Body semantics: setting an empty string is the canonical "clear
/// the body" path — the projection records exactly what the
/// amendment specifies. A true tri-state ("leave" vs "clear to
/// None" vs "set to s") would require a custom serde shape and
/// gives back only the ability to distinguish `Some("")` from
/// `None` on the projection's body field, which no observer
/// actually depends on (board renderers treat both as "no body").
/// If that distinction ever matters, swap this field for a tagged
/// enum (`BodyAmendment::Leave | Clear | Set(String)`) — the wire
/// shape can change behind serde's `untagged` discipline.
///
/// Fields that are deliberately NOT updatable post-creation:
///   * `card_id` — identity (would defeat the whole point of cards
///     being stable references).
///   * `repo` — cards live where the work lives; cross-repo
///     migration is its own card type.
///   * `created_by`, `created_at_ms` — attribution / temporal
///     anchors, append-only.
///   * `reviews` — typed sibling link set at creation; rebinding
///     a review to a different parent is structurally a different
///     review card.
///   * `lane_id` — lane membership is a separate event (lane
///     reassignment is its own concern).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardUpdated {
    pub card_id: WorkCardId,
    /// New title, if changing. `None` leaves the existing title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// New body, if changing. `None` leaves the existing body
    /// alone; `Some(s)` sets it (empty string clears in practice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// New priority, if changing. `None` leaves the existing
    /// priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Peer that emitted the amendment. Audit / attribution.
    pub updated_by: PeerId,
    pub updated_at_ms: u64,
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

/// A commit was observed by a local git adapter, CI adapter, or
/// external forge adapter. This is an observation event, not a command
/// to mutate git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitObserved {
    pub repo: RepoId,
    pub commit: GitObjectId,
    pub branch: Option<BranchName>,
    pub summary: Option<String>,
    pub observed_by: PeerId,
    pub observed_at_ms: u64,
}

/// A branch head moved. Consumers can subscribe to this instead of
/// polling `git fetch && git rev-parse` in every runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchMoved {
    pub repo: RepoId,
    pub branch: BranchName,
    pub old_head: Option<GitObjectId>,
    pub new_head: GitObjectId,
    pub moved_by: PeerId,
    pub moved_at_ms: u64,
}

/// Worktree dirty state changed. This is intentionally small and
/// closed: detailed path inventories belong in a follow-up inventory
/// event, while this event is what monitors need to wake up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDirtyStateChanged {
    pub repo: RepoId,
    pub workspace_id: Option<WorkspaceId>,
    pub path: String,
    pub state: DirtyState,
    pub dirty_paths: u64,
    pub untracked_paths: u64,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestCheckSuiteChanged {
    pub pull_request: PullRequestRef,
    pub state: PrCheckState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestReviewSubmitted {
    pub pull_request: PullRequestRef,
    pub reviewer: PeerId,
    pub state: PrReviewState,
    pub submitted_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestMergeStateChanged {
    pub pull_request: PullRequestRef,
    pub state: PrMergeState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAvailabilityReported {
    pub repo: RepoId,
    pub peer: PeerId,
    pub state: AgentAvailabilityState,
    pub note: Option<String>,
    pub ttl_ms: u64,
    pub reported_at_ms: u64,
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
            reviews: None,
            origin: None,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "card_created");
        assert_eq!(event.occurred_at_ms(), 10);
    }
}
