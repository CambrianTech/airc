use super::*;
use crate::event::*;
use crate::model::{BranchName, CardState, Priority, PullRequestRef, WorkspaceStatus};

fn repo() -> RepoId {
    RepoId::new("CambrianTech/airc").unwrap()
}

fn peer(seed: u128) -> PeerId {
    PeerId::from_u128(seed)
}

#[test]
fn card_claim_heartbeat_and_stale_detection_project_from_events() {
    let card_id = WorkCardId::from_u128(1);
    let claim_id = ClaimId::from_u128(2);
    let owner = peer(3);
    let mut projection = WorkBoardProjection::new();

    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "typed work domain".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 100,
        }))
        .unwrap();
    projection
        .apply(&WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id,
            owner,
            ttl_ms: 100,
            claimed_at_ms: 110,
        }))
        .unwrap();

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.state, CardState::Claimed);
    assert_eq!(card.claim_expires_at_ms, Some(210));
    assert!(projection.stale_claims(209).is_empty());
    assert_eq!(projection.stale_claims(210).len(), 1);

    projection
        .apply(&WorkEvent::ClaimHeartbeat(ClaimHeartbeat {
            card_id,
            claim_id,
            owner,
            ttl_ms: 100,
            heartbeat_at_ms: 200,
        }))
        .unwrap();
    assert!(projection.stale_claims(299).is_empty());
    assert_eq!(projection.stale_claims(300)[0].claim_id, claim_id);
}

#[test]
fn lane_card_and_pr_merge_projection_is_deterministic() {
    let repo = repo();
    let lane_id = LaneId::from_u128(10);
    let card_id = WorkCardId::from_u128(11);
    let owner = peer(12);
    let pr = PullRequestRef {
        repo: repo.clone(),
        number: 693,
        head: BranchName::new("feat/airc-work-domain").unwrap(),
        base: BranchName::new("rust-rewrite").unwrap(),
    };
    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::LaneCreated(LaneCreated {
            lane_id,
            repo: repo.clone(),
            title: "Work coordination domain".to_string(),
            state: LaneState::Active,
            created_by: owner,
            created_at_ms: 1,
        }),
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo,
            title: "Create airc-work".to_string(),
            body: None,
            priority: Priority::P0,
            lane_id: Some(lane_id),
            created_by: owner,
            created_at_ms: 2,
        }),
        WorkEvent::PullRequestLinked(PullRequestLinked {
            card_id,
            pull_request: pr.clone(),
            linked_by: owner,
            linked_at_ms: 3,
        }),
        WorkEvent::PullRequestMerged(PullRequestMerged {
            card_id,
            pull_request: pr.clone(),
            merged_by: owner,
            merged_at_ms: 4,
        }),
    ])
    .unwrap();

    let snapshot = projection.snapshot();
    assert_eq!(snapshot.lanes[0].card_ids, vec![card_id]);
    let card = projection.card(card_id).unwrap();
    assert_eq!(card.state, CardState::Merged);
    assert_eq!(card.pull_request, Some(pr));
}

#[test]
fn workspace_lease_lifecycle_projects_status_and_disk() {
    let card_id = WorkCardId::from_u128(1);
    let claim_id = ClaimId::from_u128(2);
    let workspace_id = WorkspaceId::from_u128(3);
    let owner = peer(4);
    let repo = repo();
    let mut projection = WorkBoardProjection::new();
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo.clone(),
            title: "workspace lifecycle".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            created_by: owner,
            created_at_ms: 1,
        }))
        .unwrap();
    projection
        .apply(&WorkEvent::WorkspaceRequested(WorkspaceRequested {
            workspace_id,
            card_id,
            claim_id,
            owner,
            repo,
            branch: BranchName::new("feat/workspace").unwrap(),
            base: BranchName::new("rust-rewrite").unwrap(),
            requested_at_ms: 2,
        }))
        .unwrap();
    projection
        .apply(&WorkEvent::WorkspaceAllocated(WorkspaceAllocated {
            workspace_id,
            path: "/tmp/airc-worktrees/ws".to_string(),
            allocated_at_ms: 3,
        }))
        .unwrap();
    projection
        .apply(&WorkEvent::WorkspaceHeartbeat(WorkspaceHeartbeat {
            workspace_id,
            disk_bytes: Some(4096),
            heartbeat_at_ms: 4,
        }))
        .unwrap();
    projection
        .apply(&WorkEvent::WorkspaceReleased(WorkspaceReleased {
            workspace_id,
            released_at_ms: 5,
        }))
        .unwrap();

    let workspace = projection.workspace(workspace_id).unwrap();
    assert_eq!(workspace.lease.status, WorkspaceStatus::Released);
    assert_eq!(workspace.lease.disk_bytes, Some(4096));
    assert_eq!(workspace.lease.released_at_ms, Some(5));
}

#[test]
fn mismatched_claim_is_rejected() {
    let card_id = WorkCardId::from_u128(1);
    let owner = peer(3);
    let mut projection = WorkBoardProjection::new();
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "claim mismatch".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            created_by: owner,
            created_at_ms: 1,
        }))
        .unwrap();
    let err = projection
        .apply(&WorkEvent::ClaimHeartbeat(ClaimHeartbeat {
            card_id,
            claim_id: ClaimId::from_u128(9),
            owner,
            ttl_ms: 1,
            heartbeat_at_ms: 2,
        }))
        .unwrap_err();
    assert!(matches!(err, ProjectionError::ClaimMismatch { .. }));
}
