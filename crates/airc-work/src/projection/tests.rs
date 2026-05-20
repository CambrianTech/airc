use super::*;
use crate::event::*;
use crate::model::{
    BranchName, CardState, DrainCandidate, DrainCandidateCategory, DrainOutcome, PressureLevel,
    Priority, PullRequestRef, WorkspaceStatus,
};

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
fn pressure_then_drain_request_then_completion_flows_through_projection() {
    let workspace_id = WorkspaceId::from_u128(42);
    let reporter = peer(7);
    let mut projection = WorkBoardProjection::new();

    // A reporter emits pressure on a workspace_id with no lease record.
    // The projection accepts it — pressure is keyed by workspace_id
    // independent of the card+claim lease flow.
    projection
        .apply(&WorkEvent::WorkspacePressureReported(
            WorkspacePressureReported {
                workspace_id,
                repo: repo(),
                reporter,
                total_bytes: 1_000_000_000,
                available_bytes: 50_000_000,
                level: PressureLevel::High,
                reported_at_ms: 1,
            },
        ))
        .unwrap();
    assert_eq!(
        projection.workspace_pressure(&workspace_id).unwrap().level,
        PressureLevel::High,
    );

    // Policy emits a drain request listing what would be reclaimed.
    let candidates = vec![
        DrainCandidate {
            path: "/tmp/work/target".to_string(),
            category: DrainCandidateCategory::RebuildableCache,
            est_bytes: 800_000_000,
        },
        DrainCandidate {
            path: "/tmp/work/.gradle".to_string(),
            category: DrainCandidateCategory::DownloadedDependency,
            est_bytes: 100_000_000,
        },
    ];
    projection
        .apply(&WorkEvent::WorkspaceDrainRequested(
            WorkspaceDrainRequested {
                workspace_id,
                repo: repo(),
                requester: reporter,
                policy_rule_id: "default.rebuildable".to_string(),
                dry_run: false,
                candidates: candidates.clone(),
                requested_at_ms: 2,
            },
        ))
        .unwrap();
    let pending = projection.pending_drains_for(&workspace_id);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].candidates, candidates);

    // Cleanup completes. Honest outcome — partial success modeled.
    projection
        .apply(&WorkEvent::WorkspaceDrainCompleted(
            WorkspaceDrainCompleted {
                workspace_id,
                repo: repo(),
                performer: reporter,
                policy_rule_id: "default.rebuildable".to_string(),
                dry_run: false,
                outcome: DrainOutcome {
                    bytes_reclaimed: 750_000_000,
                    paths_touched: vec!["/tmp/work/target".to_string()],
                    paths_skipped: vec!["/tmp/work/.gradle".to_string()],
                    errors: vec!["gradle lock contention".to_string()],
                },
                completed_at_ms: 3,
            },
        ))
        .unwrap();

    assert!(
        projection.pending_drains_for(&workspace_id).is_empty(),
        "completion must clear matching pending request",
    );
    let history = projection.drain_history_for(&workspace_id);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].outcome.bytes_reclaimed, 750_000_000);
    assert_eq!(history[0].outcome.paths_skipped, vec!["/tmp/work/.gradle"]);
    assert_eq!(history[0].outcome.errors.len(), 1);
}

#[test]
fn drain_candidate_safe_by_default_is_conservative() {
    assert!(DrainCandidateCategory::RebuildableCache.safe_by_default());
    assert!(DrainCandidateCategory::DownloadedDependency.safe_by_default());
    // Everything else requires opt-in. No silent destruction.
    assert!(!DrainCandidateCategory::GeneratedArtifact.safe_by_default());
    assert!(!DrainCandidateCategory::DockerLayer.safe_by_default());
    assert!(!DrainCandidateCategory::ModelCache.safe_by_default());
    assert!(!DrainCandidateCategory::TraceArtifact.safe_by_default());
    assert!(!DrainCandidateCategory::Unknown.safe_by_default());
}

#[test]
fn two_concurrent_drain_requests_for_same_workspace_and_rule_overwrites() {
    // Surfacing policy bug by overwrite (not erroring) — the projection
    // is a derived view, errors here would hide the bug from observers.
    let workspace_id = WorkspaceId::from_u128(99);
    let mut projection = WorkBoardProjection::new();
    let req = |requested_at_ms| WorkspaceDrainRequested {
        workspace_id,
        repo: repo(),
        requester: peer(1),
        policy_rule_id: "default.rebuildable".to_string(),
        dry_run: false,
        candidates: vec![],
        requested_at_ms,
    };
    projection
        .apply(&WorkEvent::WorkspaceDrainRequested(req(10)))
        .unwrap();
    projection
        .apply(&WorkEvent::WorkspaceDrainRequested(req(20)))
        .unwrap();
    let pending = projection.pending_drains_for(&workspace_id);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].requested_at_ms, 20);
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
