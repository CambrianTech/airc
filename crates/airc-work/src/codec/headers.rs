//! Header projection for work-domain events.

use airc_core::Headers;
use airc_protocol::HEADER_FORGE_BODY_HINT;

use crate::codec::{event_kind, BODY_HINT_FORGE_WORK_EVENT};
use crate::event::WorkEvent;

pub const HEADER_FORGE_WORK_EVENT_KIND: &str = "forge.work.kind";
pub const HEADER_FORGE_WORK_REPO: &str = "forge.work.repo";
pub const HEADER_FORGE_WORK_CARD_ID: &str = "forge.work.card_id";
pub const HEADER_FORGE_WORK_LANE_ID: &str = "forge.work.lane_id";
pub const HEADER_FORGE_WORK_CLAIM_ID: &str = "forge.work.claim_id";
pub const HEADER_FORGE_WORK_WORKSPACE_ID: &str = "forge.work.workspace_id";
pub const HEADER_FORGE_WORK_POLICY_RULE_ID: &str = "forge.work.policy_rule_id";
pub const HEADER_FORGE_WORK_GIT_BRANCH: &str = "forge.work.git_branch";
pub const HEADER_FORGE_WORK_GIT_COMMIT: &str = "forge.work.git_commit";
pub const HEADER_FORGE_WORK_PR_NUMBER: &str = "forge.work.pr_number";

pub fn work_event_headers(event: &WorkEvent) -> Headers {
    let mut headers = Headers::new();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_WORK_EVENT.to_string(),
    );
    headers.insert(
        HEADER_FORGE_WORK_EVENT_KIND.to_string(),
        event_kind(event).to_string(),
    );
    project_domain_headers(event, &mut headers);
    headers
}

fn project_domain_headers(event: &WorkEvent, headers: &mut Headers) {
    match event {
        WorkEvent::CardCreated(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_CARD_ID, e.card_id);
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
            if let Some(lane_id) = e.lane_id {
                insert_display_header(headers, HEADER_FORGE_WORK_LANE_ID, lane_id);
            }
        }
        WorkEvent::CardClaimed(e) => project_claim(headers, e.card_id, e.claim_id),
        WorkEvent::ClaimHeartbeat(e) => project_claim(headers, e.card_id, e.claim_id),
        WorkEvent::ClaimReleased(e) => project_claim(headers, e.card_id, e.claim_id),
        WorkEvent::CardStateChanged(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_CARD_ID, e.card_id);
        }
        WorkEvent::LaneCreated(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_LANE_ID, e.lane_id);
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
        }
        WorkEvent::LaneStateChanged(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_LANE_ID, e.lane_id);
        }
        WorkEvent::WorkspaceRequested(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, e.workspace_id);
            project_claim(headers, e.card_id, e.claim_id);
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
        }
        WorkEvent::WorkspaceAllocated(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, e.workspace_id);
        }
        WorkEvent::WorkspaceHeartbeat(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, e.workspace_id);
        }
        WorkEvent::WorkspaceReleased(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, e.workspace_id);
        }
        WorkEvent::WorkspacePressureReported(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, e.workspace_id);
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
        }
        WorkEvent::WorkspaceDrainRequested(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, e.workspace_id);
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
            headers.insert(
                HEADER_FORGE_WORK_POLICY_RULE_ID.to_string(),
                e.policy_rule_id.clone(),
            );
        }
        WorkEvent::WorkspaceDrainCompleted(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, e.workspace_id);
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
            headers.insert(
                HEADER_FORGE_WORK_POLICY_RULE_ID.to_string(),
                e.policy_rule_id.clone(),
            );
        }
        WorkEvent::GitCommitObserved(e) => {
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
            insert_display_header(headers, HEADER_FORGE_WORK_GIT_COMMIT, &e.commit);
            if let Some(branch) = &e.branch {
                insert_display_header(headers, HEADER_FORGE_WORK_GIT_BRANCH, branch);
            }
        }
        WorkEvent::GitBranchMoved(e) => {
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
            insert_display_header(headers, HEADER_FORGE_WORK_GIT_BRANCH, &e.branch);
            insert_display_header(headers, HEADER_FORGE_WORK_GIT_COMMIT, &e.new_head);
        }
        WorkEvent::GitDirtyStateChanged(e) => {
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
            if let Some(workspace_id) = e.workspace_id {
                insert_display_header(headers, HEADER_FORGE_WORK_WORKSPACE_ID, workspace_id);
            }
        }
        WorkEvent::PullRequestCheckSuiteChanged(e) => {
            project_pull_request(headers, &e.pull_request);
        }
        WorkEvent::PullRequestReviewSubmitted(e) => {
            project_pull_request(headers, &e.pull_request);
        }
        WorkEvent::PullRequestMergeStateChanged(e) => {
            project_pull_request(headers, &e.pull_request);
        }
        WorkEvent::PullRequestLinked(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_CARD_ID, e.card_id);
            project_pull_request(headers, &e.pull_request);
        }
        WorkEvent::PullRequestMerged(e) => {
            insert_display_header(headers, HEADER_FORGE_WORK_CARD_ID, e.card_id);
            project_pull_request(headers, &e.pull_request);
        }
        WorkEvent::HygieneReportRecorded(e) => {
            headers.insert(
                HEADER_FORGE_WORK_REPO.to_string(),
                e.report.repo.to_string(),
            );
        }
        WorkEvent::ManagerHatClaimed(e) => {
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
        }
        WorkEvent::ManagerHatReleased(e) => {
            headers.insert(HEADER_FORGE_WORK_REPO.to_string(), e.repo.to_string());
        }
    }
}

fn project_claim(headers: &mut Headers, card_id: crate::WorkCardId, claim_id: crate::ClaimId) {
    insert_display_header(headers, HEADER_FORGE_WORK_CARD_ID, card_id);
    insert_display_header(headers, HEADER_FORGE_WORK_CLAIM_ID, claim_id);
}

fn project_pull_request(headers: &mut Headers, pull_request: &crate::PullRequestRef) {
    headers.insert(
        HEADER_FORGE_WORK_REPO.to_string(),
        pull_request.repo.to_string(),
    );
    insert_display_header(headers, HEADER_FORGE_WORK_PR_NUMBER, pull_request.number);
    insert_display_header(headers, HEADER_FORGE_WORK_GIT_BRANCH, &pull_request.head);
}

fn insert_display_header(headers: &mut Headers, key: &str, value: impl std::fmt::Display) {
    headers.insert(key.to_string(), value.to_string());
}
