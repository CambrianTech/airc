use airc_core::{Body, PeerId};
use airc_protocol::{FrameKind, HEADER_FORGE_BODY_HINT};

use super::*;
use crate::{
    BranchName, CardCreated, ClaimId, DrainCandidate, DrainCandidateCategory, DrainOutcome,
    GitBranchMoved, GitObjectId, LaneId, PrCheckState, PressureLevel, Priority,
    PullRequestCheckSuiteChanged, PullRequestRef, RepoId, WorkCardId, WorkEvent,
    WorkspaceDrainCompleted, WorkspaceDrainRequested, WorkspaceId, WorkspacePressureReported,
    WorkspaceRequested,
};

fn card_created() -> WorkEvent {
    WorkEvent::CardCreated(CardCreated {
        card_id: WorkCardId::from_u128(1),
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "make work events routable".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: Some(LaneId::from_u128(2)),
        created_by: PeerId::from_u128(3),
        created_at_ms: 4,
    })
}

#[test]
fn work_event_roundtrips_through_headers_and_body() {
    let event = card_created();

    let (headers, body) = encode_work_event(&event).unwrap();
    let decoded = decode_work_event(&headers, Some(&body)).unwrap();

    assert_eq!(decoded, event);
    assert_eq!(
        headers.get(HEADER_FORGE_BODY_HINT).map(String::as_str),
        Some(BODY_HINT_FORGE_WORK_EVENT)
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("card_created")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/airc")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CARD_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-000000000001")
    );
}

#[test]
fn subscription_matches_work_events_without_parsing_body() {
    let event = card_created();
    let (headers, _) = encode_work_event(&event).unwrap();
    let sub = work_event_subscription();

    assert!(sub.headers_filter.matches(&headers));
    assert!(sub.kinds.contains(&FrameKind::Event));
}

#[test]
fn decode_rejects_wrong_hint_and_non_json_body() {
    let event = card_created();
    let (mut headers, _) = encode_work_event(&event).unwrap();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        "forge.chat.text".to_string(),
    );

    assert!(matches!(
        decode_work_event(&headers, Some(&Body::text("not work"))),
        Err(WorkEventCodecError::BodyHintMismatch { .. })
    ));

    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_WORK_EVENT.to_string(),
    );
    assert!(matches!(
        decode_work_event(&headers, Some(&Body::Binary(vec![1, 2, 3]))),
        Err(WorkEventCodecError::NonJsonBody)
    ));
}

#[test]
fn drain_events_roundtrip_and_carry_workspace_repo_and_policy_rule_headers() {
    let workspace_id = WorkspaceId::from_u128(0xa1);
    let repo = RepoId::new("CambrianTech/airc").unwrap();
    let reporter = PeerId::from_u128(0xb2);
    let policy_rule_id = "default.rebuildable".to_string();

    let pressure = WorkEvent::WorkspacePressureReported(WorkspacePressureReported {
        workspace_id,
        repo: repo.clone(),
        reporter,
        total_bytes: 1_000_000,
        available_bytes: 50_000,
        level: PressureLevel::High,
        reported_at_ms: 1,
    });
    let (headers, body) = encode_work_event(&pressure).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), pressure);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("workspace_pressure_reported")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_WORKSPACE_ID)
            .map(String::as_str),
        Some("00000000-0000-0000-0000-0000000000a1")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/airc")
    );

    let request = WorkEvent::WorkspaceDrainRequested(WorkspaceDrainRequested {
        workspace_id,
        repo: repo.clone(),
        requester: reporter,
        policy_rule_id: policy_rule_id.clone(),
        dry_run: true,
        candidates: vec![DrainCandidate {
            path: "/tmp/work/target".to_string(),
            category: DrainCandidateCategory::RebuildableCache,
            est_bytes: 500_000,
        }],
        requested_at_ms: 2,
    });
    let (headers, body) = encode_work_event(&request).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), request);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("workspace_drain_requested")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_POLICY_RULE_ID)
            .map(String::as_str),
        Some("default.rebuildable")
    );

    let completed = WorkEvent::WorkspaceDrainCompleted(WorkspaceDrainCompleted {
        workspace_id,
        repo,
        performer: reporter,
        policy_rule_id: policy_rule_id.clone(),
        dry_run: false,
        outcome: DrainOutcome {
            bytes_reclaimed: 450_000,
            paths_touched: vec!["/tmp/work/target".to_string()],
            paths_skipped: vec![],
            errors: vec![],
        },
        completed_at_ms: 3,
    });
    let (headers, body) = encode_work_event(&completed).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), completed);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("workspace_drain_completed")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_POLICY_RULE_ID)
            .map(String::as_str),
        Some("default.rebuildable")
    );
}

#[test]
fn workspace_headers_include_workspace_claim_card_and_repo() {
    let event = WorkEvent::WorkspaceRequested(WorkspaceRequested {
        workspace_id: WorkspaceId::from_u128(10),
        card_id: WorkCardId::from_u128(11),
        claim_id: ClaimId::from_u128(12),
        owner: PeerId::from_u128(13),
        repo: RepoId::new("CambrianTech/continuum").unwrap(),
        branch: BranchName::new("feat/rust-work").unwrap(),
        base: BranchName::new("rust-rewrite").unwrap(),
        requested_at_ms: 14,
    });

    let headers = work_event_headers(&event);

    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_WORKSPACE_ID)
            .map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000a")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CARD_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000b")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CLAIM_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000c")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/continuum")
    );
}

#[test]
fn git_and_pr_events_roundtrip_with_routeable_headers() {
    let repo = RepoId::new("CambrianTech/airc").unwrap();
    let branch = BranchName::new("rust-rewrite").unwrap();
    let commit = GitObjectId::new("abc123").unwrap();

    let git_event = WorkEvent::GitBranchMoved(GitBranchMoved {
        repo: repo.clone(),
        branch: branch.clone(),
        old_head: None,
        new_head: commit.clone(),
        moved_by: PeerId::from_u128(1),
        moved_at_ms: 2,
    });
    let (headers, body) = encode_work_event(&git_event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), git_event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("git_branch_moved")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_GIT_BRANCH)
            .map(String::as_str),
        Some("rust-rewrite")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_GIT_COMMIT)
            .map(String::as_str),
        Some("abc123")
    );

    let pr_event = WorkEvent::PullRequestCheckSuiteChanged(PullRequestCheckSuiteChanged {
        pull_request: PullRequestRef {
            repo,
            number: 914,
            head: BranchName::new("feat/lifecycle-events").unwrap(),
            base: branch,
        },
        state: PrCheckState::Passed,
        changed_by: PeerId::from_u128(3),
        changed_at_ms: 4,
    });
    let (headers, body) = encode_work_event(&pr_event).unwrap();
    assert_eq!(decode_work_event(&headers, Some(&body)).unwrap(), pr_event);
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("pull_request_check_suite_changed")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_PR_NUMBER).map(String::as_str),
        Some("914")
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_GIT_BRANCH)
            .map(String::as_str),
        Some("feat/lifecycle-events")
    );
}
