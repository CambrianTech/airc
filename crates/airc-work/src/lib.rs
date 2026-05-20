//! `airc-work` — typed work coordination domain for AIRC.
//!
//! This crate is the Rust contract for queue cards, lanes, work claims,
//! workspaces, PR links, hygiene reports, and manager-hat state. GitHub
//! issues/PRs are adapters that mirror these events; they are not the
//! runtime source of truth.

pub mod codec;
pub mod event;
pub mod ids;
pub mod model;
pub mod projection;
pub mod replay;

pub use codec::{
    decode_work_event, encode_work_event, work_event_headers, work_event_subscription,
    WorkEventCodecError, BODY_HINT_FORGE_WORK_EVENT, HEADER_FORGE_WORK_CARD_ID,
    HEADER_FORGE_WORK_CLAIM_ID, HEADER_FORGE_WORK_EVENT_KIND, HEADER_FORGE_WORK_LANE_ID,
    HEADER_FORGE_WORK_POLICY_RULE_ID, HEADER_FORGE_WORK_REPO, HEADER_FORGE_WORK_WORKSPACE_ID,
};
pub use event::{
    CardCreated, CardStateChanged, ClaimHeartbeat, ClaimReleased, HygieneReportRecorded,
    LaneCreated, LaneStateChanged, ManagerHatClaimed, ManagerHatReleased, PullRequestLinked,
    PullRequestMerged, WorkCardClaimed, WorkEvent, WorkspaceAllocated, WorkspaceDrainCompleted,
    WorkspaceDrainRequested, WorkspaceHeartbeat, WorkspacePressureReported, WorkspaceReleased,
    WorkspaceRequested,
};
pub use ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};
pub use model::{
    BranchName, CardState, DrainCandidate, DrainCandidateCategory, DrainOutcome, HygieneReport,
    LaneState, PressureLevel, Priority, PullRequestRef, WorkCard, WorkspaceLease, WorkspaceStatus,
};
pub use projection::{
    BoardSnapshot, LaneRecord, ManagerHat, ProjectionError, StaleClaim, WorkBoardProjection,
    WorkspaceRecord,
};
pub use replay::{
    decode_transcript_work_event, project_transcript_work_events, transcript_is_work_event,
    WorkReplayError, WorkReplayItem,
};
