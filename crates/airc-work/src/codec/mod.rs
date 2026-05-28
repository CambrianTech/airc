//! Header/body codec for work-domain events.
//!
//! AIRC routes on cheap envelope headers. The work event itself stays in
//! the opaque body so substrate transports do not need to understand
//! kanban, workspaces, GitHub, Codex, Continuum, OpenClaw, or Hermes.

use std::collections::BTreeSet;

use airc_core::{Body, HeaderFilter, Headers};
use airc_protocol::{FrameKind, Subscription, HEADER_FORGE_BODY_HINT};

use crate::event::WorkEvent;

mod headers;
#[cfg(test)]
mod tests;

pub use headers::{
    work_event_headers, HEADER_FORGE_WORK_CARD_ID, HEADER_FORGE_WORK_CLAIM_ID,
    HEADER_FORGE_WORK_EVENT_KIND, HEADER_FORGE_WORK_GIT_BRANCH, HEADER_FORGE_WORK_GIT_COMMIT,
    HEADER_FORGE_WORK_LANE_ID, HEADER_FORGE_WORK_POLICY_RULE_ID, HEADER_FORGE_WORK_PR_NUMBER,
    HEADER_FORGE_WORK_REPO, HEADER_FORGE_WORK_STATE, HEADER_FORGE_WORK_WORKSPACE_ID,
};

pub const BODY_HINT_FORGE_WORK_EVENT: &str = "forge.work.event.v1";

#[derive(Debug, thiserror::Error)]
pub enum WorkEventCodecError {
    #[error("work event body is missing")]
    MissingBody,
    #[error("work event body must be JSON")]
    NonJsonBody,
    #[error("work event body uses hint {actual:?}, expected {expected:?}")]
    BodyHintMismatch {
        actual: Option<String>,
        expected: &'static str,
    },
    #[error("work event JSON codec failed: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn encode_work_event(event: &WorkEvent) -> Result<(Headers, Body), WorkEventCodecError> {
    Ok((
        work_event_headers(event),
        Body::Json(serde_json::to_value(event)?),
    ))
}

pub fn decode_work_event(
    headers: &Headers,
    body: Option<&Body>,
) -> Result<WorkEvent, WorkEventCodecError> {
    require_work_event_hint(headers)?;
    let Some(body) = body else {
        return Err(WorkEventCodecError::MissingBody);
    };
    let Body::Json(value) = body else {
        return Err(WorkEventCodecError::NonJsonBody);
    };
    Ok(serde_json::from_value(value.clone())?)
}

pub fn work_event_subscription() -> Subscription {
    Subscription {
        kinds: BTreeSet::from([FrameKind::Event]),
        headers_filter: HeaderFilter::Exact {
            key: HEADER_FORGE_BODY_HINT.to_string(),
            value: BODY_HINT_FORGE_WORK_EVENT.to_string(),
        },
        ..Default::default()
    }
}

fn require_work_event_hint(headers: &Headers) -> Result<(), WorkEventCodecError> {
    match headers.get(HEADER_FORGE_BODY_HINT) {
        Some(value) if value == BODY_HINT_FORGE_WORK_EVENT => Ok(()),
        actual => Err(WorkEventCodecError::BodyHintMismatch {
            actual: actual.cloned(),
            expected: BODY_HINT_FORGE_WORK_EVENT,
        }),
    }
}

pub(crate) fn event_kind(event: &WorkEvent) -> &'static str {
    match event {
        WorkEvent::CardCreated(_) => "card_created",
        WorkEvent::CardUpdated(_) => "card_updated",
        WorkEvent::CardClaimed(_) => "card_claimed",
        WorkEvent::ClaimHeartbeat(_) => "claim_heartbeat",
        WorkEvent::ClaimReleased(_) => "claim_released",
        WorkEvent::CardStateChanged(_) => "card_state_changed",
        WorkEvent::LaneCreated(_) => "lane_created",
        WorkEvent::LaneStateChanged(_) => "lane_state_changed",
        WorkEvent::WorkspaceRequested(_) => "workspace_requested",
        WorkEvent::WorkspaceAllocated(_) => "workspace_allocated",
        WorkEvent::WorkspaceHeartbeat(_) => "workspace_heartbeat",
        WorkEvent::WorkspaceReleased(_) => "workspace_released",
        WorkEvent::WorkspacePressureReported(_) => "workspace_pressure_reported",
        WorkEvent::WorkspaceDrainRequested(_) => "workspace_drain_requested",
        WorkEvent::WorkspaceDrainCompleted(_) => "workspace_drain_completed",
        WorkEvent::GitCommitObserved(_) => "git_commit_observed",
        WorkEvent::GitBranchMoved(_) => "git_branch_moved",
        WorkEvent::GitDirtyStateChanged(_) => "git_dirty_state_changed",
        WorkEvent::PullRequestCheckSuiteChanged(_) => "pull_request_check_suite_changed",
        WorkEvent::PullRequestReviewSubmitted(_) => "pull_request_review_submitted",
        WorkEvent::PullRequestMergeStateChanged(_) => "pull_request_merge_state_changed",
        WorkEvent::PullRequestLinked(_) => "pull_request_linked",
        WorkEvent::PullRequestMerged(_) => "pull_request_merged",
        WorkEvent::HygieneReportRecorded(_) => "hygiene_report_recorded",
        WorkEvent::ManagerHatClaimed(_) => "manager_hat_claimed",
        WorkEvent::ManagerHatReleased(_) => "manager_hat_released",
        WorkEvent::AgentAvailabilityReported(_) => "agent_availability_reported",
    }
}
