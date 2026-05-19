//! `airc-work` — typed work coordination domain for AIRC.
//!
//! This crate is the Rust contract for queue cards, lanes, work claims,
//! workspaces, PR links, hygiene reports, and manager-hat state. GitHub
//! issues/PRs are adapters that mirror these events; they are not the
//! runtime source of truth.

pub mod event;
pub mod ids;
pub mod model;
pub mod projection;

pub use event::{
    CardCreated, CardStateChanged, ClaimHeartbeat, ClaimReleased, HygieneReportRecorded,
    LaneCreated, LaneStateChanged, ManagerHatClaimed, ManagerHatReleased, PullRequestLinked,
    PullRequestMerged, WorkCardClaimed, WorkEvent, WorkspaceAllocated, WorkspaceHeartbeat,
    WorkspaceReleased, WorkspaceRequested,
};
pub use ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};
pub use model::{
    BranchName, CardState, HygieneReport, LaneState, Priority, PullRequestRef, WorkCard,
    WorkspaceLease, WorkspaceStatus,
};
pub use projection::{
    BoardSnapshot, LaneRecord, ProjectionError, StaleClaim, WorkBoardProjection, WorkspaceRecord,
};
