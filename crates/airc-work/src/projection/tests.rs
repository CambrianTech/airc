use super::*;
use crate::event::*;
use crate::model::{
    AgentAvailabilityState, BranchName, CardState, DirtyState, DrainCandidate,
    DrainCandidateCategory, DrainOutcome, GitObjectId, PrCheckState, PrMergeState, PrReviewState,
    PressureLevel, Priority, PullRequestRef, WorkspaceStatus,
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
fn terminal_cards_do_not_surface_stale_claims() {
    let owner = peer(6);
    let merged_card = WorkCardId::from_u128(7);
    let merged_claim = ClaimId::from_u128(8);
    let closed_card = WorkCardId::from_u128(9);
    let closed_claim = ClaimId::from_u128(10);

    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::CardCreated(CardCreated {
            card_id: merged_card,
            repo: repo(),
            title: "merged stale claim".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 100,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id: merged_card,
            claim_id: merged_claim,
            owner,
            ttl_ms: 10,
            claimed_at_ms: 110,
        }),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id: merged_card,
            state: CardState::Merged,
            changed_by: owner,
            changed_at_ms: 120,
        }),
        WorkEvent::CardCreated(CardCreated {
            card_id: closed_card,
            repo: repo(),
            title: "closed stale claim".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 100,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id: closed_card,
            claim_id: closed_claim,
            owner,
            ttl_ms: 10,
            claimed_at_ms: 110,
        }),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id: closed_card,
            state: CardState::Closed,
            changed_by: owner,
            changed_at_ms: 120,
        }),
    ])
    .unwrap();

    assert!(projection.stale_claims(121).is_empty());
}

#[test]
fn releasing_claim_clears_owner_without_reopening_closed_card() {
    let card_id = WorkCardId::from_u128(10);
    let claim_id = ClaimId::from_u128(11);
    let owner = peer(12);

    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "close before release".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 100,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id,
            owner,
            ttl_ms: 100,
            claimed_at_ms: 110,
        }),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id,
            state: CardState::Closed,
            changed_by: owner,
            changed_at_ms: 120,
        }),
        WorkEvent::ClaimReleased(ClaimReleased {
            card_id,
            claim_id,
            owner,
            reason: Some("merged".to_string()),
            released_at_ms: 130,
        }),
    ])
    .unwrap();

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.state, CardState::Closed);
    assert_eq!(card.owner, None);
    assert_eq!(card.claim_id, None);
}

#[test]
fn duplicate_claim_release_is_idempotent_after_claim_is_already_clear() {
    let card_id = WorkCardId::from_u128(13);
    let claim_id = ClaimId::from_u128(14);
    let owner = peer(15);

    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "duplicate release".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 100,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id,
            owner,
            ttl_ms: 100,
            claimed_at_ms: 110,
        }),
        WorkEvent::ClaimReleased(ClaimReleased {
            card_id,
            claim_id,
            owner,
            reason: Some("first release".to_string()),
            released_at_ms: 120,
        }),
        WorkEvent::ClaimReleased(ClaimReleased {
            card_id,
            claim_id,
            owner,
            reason: Some("duplicate release".to_string()),
            released_at_ms: 130,
        }),
    ])
    .unwrap();

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.state, CardState::Open);
    assert_eq!(card.owner, None);
    assert_eq!(card.claim_id, None);
}

#[test]
fn duplicate_active_claim_is_idempotent_and_keeps_original_owner() {
    let card_id = WorkCardId::from_u128(16);
    let first_claim = ClaimId::from_u128(17);
    let second_claim = ClaimId::from_u128(18);
    let first_owner = peer(19);
    let second_owner = peer(20);

    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "duplicate active claim".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: first_owner,
            created_at_ms: 100,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: first_claim,
            owner: first_owner,
            ttl_ms: 100,
            claimed_at_ms: 110,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: second_claim,
            owner: second_owner,
            ttl_ms: 100,
            claimed_at_ms: 120,
        }),
        WorkEvent::ClaimReleased(ClaimReleased {
            card_id,
            claim_id: first_claim,
            owner: first_owner,
            reason: Some("release original claim".to_string()),
            released_at_ms: 130,
        }),
    ])
    .unwrap();

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.owner, None);
    assert_eq!(card.claim_id, None);
}

#[test]
fn expired_claim_can_be_superseded_by_new_claim() {
    let card_id = WorkCardId::from_u128(21);
    let expired_claim = ClaimId::from_u128(22);
    let new_claim = ClaimId::from_u128(23);
    let expired_owner = peer(24);
    let new_owner = peer(25);

    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "recover abandoned claim".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: expired_owner,
            created_at_ms: 100,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: expired_claim,
            owner: expired_owner,
            ttl_ms: 100,
            claimed_at_ms: 110,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: new_claim,
            owner: new_owner,
            ttl_ms: 100,
            claimed_at_ms: 210,
        }),
    ])
    .unwrap();

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.owner, Some(new_owner));
    assert_eq!(card.claim_id, Some(new_claim));
    assert_eq!(card.claim_expires_at_ms, Some(310));
    assert!(projection.stale_claims(309).is_empty());
}

#[test]
fn availability_projection_keeps_latest_peer_state_per_repo() {
    let repo = repo();
    let other_repo = RepoId::new("CambrianTech/continuum").unwrap();
    let alice = peer(41);

    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::AgentAvailabilityReported(AgentAvailabilityReported {
            repo: repo.clone(),
            peer: alice,
            state: AgentAvailabilityState::Ready,
            note: Some("can review".to_string()),
            ttl_ms: 100,
            reported_at_ms: 10,
        }),
        WorkEvent::AgentAvailabilityReported(AgentAvailabilityReported {
            repo: repo.clone(),
            peer: alice,
            state: AgentAvailabilityState::Busy,
            note: Some("taking PR".to_string()),
            ttl_ms: 50,
            reported_at_ms: 20,
        }),
        WorkEvent::AgentAvailabilityReported(AgentAvailabilityReported {
            repo: other_repo.clone(),
            peer: alice,
            state: AgentAvailabilityState::Ready,
            note: None,
            ttl_ms: 100,
            reported_at_ms: 30,
        }),
    ])
    .unwrap();

    let mut availability = projection.snapshot().agent_availability;
    availability.sort_by_key(|record| record.report.repo.to_string());

    assert_eq!(availability.len(), 2);
    assert_eq!(availability[0].report.repo, repo);
    assert_eq!(availability[0].report.state, AgentAvailabilityState::Busy);
    assert_eq!(availability[0].expires_at_ms, 70);
    assert_eq!(availability[1].report.repo, other_repo);
    assert_eq!(availability[1].report.state, AgentAvailabilityState::Ready);
    assert_eq!(availability[1].expires_at_ms, 130);
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
fn git_and_pr_adapter_events_project_without_polling() {
    let repo = repo();
    let branch = BranchName::new("rust-rewrite").unwrap();
    let head = GitObjectId::new("abc123").unwrap();
    let next = GitObjectId::new("def456").unwrap();
    let peer = peer(77);
    let pr = PullRequestRef {
        repo: repo.clone(),
        number: 914,
        head: BranchName::new("feat/lifecycle-events").unwrap(),
        base: branch.clone(),
    };

    let projection = WorkBoardProjection::replay(vec![
        WorkEvent::GitCommitObserved(GitCommitObserved {
            repo: repo.clone(),
            commit: head.clone(),
            branch: Some(branch.clone()),
            summary: Some("initial rust substrate".to_string()),
            observed_by: peer,
            observed_at_ms: 10,
        }),
        WorkEvent::GitBranchMoved(GitBranchMoved {
            repo: repo.clone(),
            branch: branch.clone(),
            old_head: Some(head.clone()),
            new_head: next.clone(),
            moved_by: peer,
            moved_at_ms: 20,
        }),
        WorkEvent::GitDirtyStateChanged(GitDirtyStateChanged {
            repo: repo.clone(),
            workspace_id: Some(WorkspaceId::from_u128(5)),
            path: "/Users/joelteply/.airc/worktrees/airc/feat".to_string(),
            state: DirtyState::Dirty,
            dirty_paths: 2,
            untracked_paths: 1,
            changed_by: peer,
            changed_at_ms: 30,
        }),
        WorkEvent::PullRequestCheckSuiteChanged(PullRequestCheckSuiteChanged {
            pull_request: pr.clone(),
            state: PrCheckState::Running,
            changed_by: peer,
            changed_at_ms: 40,
        }),
        WorkEvent::PullRequestReviewSubmitted(PullRequestReviewSubmitted {
            pull_request: pr.clone(),
            reviewer: peer,
            state: PrReviewState::Approved,
            submitted_at_ms: 50,
        }),
        WorkEvent::PullRequestMergeStateChanged(PullRequestMergeStateChanged {
            pull_request: pr.clone(),
            state: PrMergeState::Merged,
            changed_by: peer,
            changed_at_ms: 60,
        }),
    ])
    .unwrap();

    let repo_tracking = projection.repo_tracking(&repo).unwrap();
    assert_eq!(repo_tracking.observed_commits.len(), 1);
    assert_eq!(repo_tracking.dirty_states[0].state, DirtyState::Dirty);
    assert_eq!(repo_tracking.branches[&branch].head, next);
    assert_eq!(repo_tracking.branches[&branch].updated_at_ms, 20);

    let pr_record = projection.pull_request(&repo, 914).unwrap();
    assert_eq!(pr_record.pull_request, pr);
    assert_eq!(pr_record.check_state, Some(PrCheckState::Running));
    assert_eq!(pr_record.review_state, Some(PrReviewState::Approved));
    assert_eq!(pr_record.merge_state, Some(PrMergeState::Merged));
    assert_eq!(pr_record.updated_at_ms, 60);

    let snapshot = projection.snapshot();
    assert_eq!(snapshot.repo_tracking.len(), 1);
    assert_eq!(snapshot.pull_requests.len(), 1);
}

#[test]
fn heartbeat_for_non_active_claim_is_dropped_silently() {
    // Closes work card c29506b8 ("Work board replay window must
    // not fail on partial history"). A heartbeat that names a
    // claim the projection has never observed as active (e.g. a
    // racing second claimant that lost the first-write-wins
    // contest in `apply_card_claimed`) must NOT poison the
    // board — drop the event and let the projection move on.
    let card_id = WorkCardId::from_u128(1);
    let owner = peer(3);
    let mut projection = WorkBoardProjection::new();
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "ghost heartbeat".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            created_by: owner,
            created_at_ms: 1,
        }))
        .unwrap();
    projection
        .apply(&WorkEvent::ClaimHeartbeat(ClaimHeartbeat {
            card_id,
            claim_id: ClaimId::from_u128(9),
            owner,
            ttl_ms: 1,
            heartbeat_at_ms: 2,
        }))
        .expect("ghost heartbeat must be tolerated, not panic");
    let card = projection.card(card_id).expect("card present");
    assert_eq!(
        card.claim_id, None,
        "ghost heartbeat must not invent a claim"
    );
}

#[test]
fn release_for_superseded_claim_does_not_poison_projection() {
    // Closes work card c29506b8 (the production poison-pill that
    // motivated this fix). Real wire shape: Create → Claim A →
    // Claim B (silently dropped by first-write-wins) → Release B.
    // The release names a claim id the projection never accepted;
    // refusing it would freeze every consumer's board.
    let card_id = WorkCardId::from_u128(1);
    let owner_a = peer(3);
    let owner_b = peer(4);
    let claim_a = ClaimId::from_u128(10);
    let claim_b = ClaimId::from_u128(11);
    let mut projection = WorkBoardProjection::new();

    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "race release".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            created_by: owner_a,
            created_at_ms: 1,
        }))
        .unwrap();
    projection
        .apply(&WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: claim_a,
            owner: owner_a,
            ttl_ms: 60_000,
            claimed_at_ms: 2,
        }))
        .unwrap();
    // Second claim — silently ignored by first-write-wins.
    projection
        .apply(&WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: claim_b,
            owner: owner_b,
            ttl_ms: 60_000,
            claimed_at_ms: 3,
        }))
        .unwrap();
    // The losing claimant later emits a release for *their* claim
    // id. Before this fix, this errored with `ClaimMismatch` and
    // halted every consumer's projection.
    projection
        .apply(&WorkEvent::ClaimReleased(ClaimReleased {
            card_id,
            claim_id: claim_b,
            owner: owner_b,
            reason: None,
            released_at_ms: 4,
        }))
        .expect("release of superseded claim must be tolerated");

    let card = projection.card(card_id).expect("card present");
    assert_eq!(
        card.claim_id,
        Some(claim_a),
        "first-write claim must still be active after the ghost release"
    );
    assert_eq!(card.owner, Some(owner_a));
    assert_eq!(card.state, CardState::Claimed);
}

// Invariant (d) of card 5d65aec2 — peer self-organizing contract:
// two peers creating cards concurrently must not collide. The
// store-as-arbiter contract for *creation* reduces to "fresh UUID per
// card" + projection's id-keyed insertion; distinct ids round-trip
// both writers, and a true id collision SURFACES (no silent clobber).

#[test]
fn concurrent_card_creates_with_distinct_ids_project_independently() {
    let card_a = WorkCardId::from_u128(101);
    let card_b = WorkCardId::from_u128(102);
    let alice = peer(10);
    let bob = peer(20);

    let mut projection = WorkBoardProjection::new();
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id: card_a,
            repo: repo(),
            title: "alice's card".into(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: alice,
            created_at_ms: 100,
        }))
        .expect("alice's create projects");
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id: card_b,
            repo: repo(),
            title: "bob's card".into(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            created_by: bob,
            created_at_ms: 101,
        }))
        .expect("bob's create projects independently");

    let a = projection.card(card_a).expect("card_a present");
    let b = projection.card(card_b).expect("card_b present");
    assert_eq!(a.title, "alice's card");
    assert_eq!(a.created_by, alice);
    assert_eq!(a.priority, Priority::P1);
    assert_eq!(b.title, "bob's card");
    assert_eq!(b.created_by, bob);
    assert_eq!(b.priority, Priority::P2);
}

#[test]
fn duplicate_card_id_on_create_surfaces_error_never_silent_clobber() {
    // A true id collision is structurally unreachable with fresh
    // UUIDs, but the arbitration contract must still be explicit:
    // silent overwrite would let a bug — or a malicious peer —
    // replace a card. Test pins "surfaces as DuplicateCard, original
    // unchanged".
    let card_id = WorkCardId::from_u128(42);
    let mut projection = WorkBoardProjection::new();
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "original".into(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: peer(1),
            created_at_ms: 100,
        }))
        .expect("first create projects");
    let err = projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "clobber attempt".into(),
            body: None,
            priority: Priority::P3,
            lane_id: None,
            created_by: peer(2),
            created_at_ms: 200,
        }))
        .expect_err("duplicate card_id must surface as an error");
    assert!(
        matches!(err, super::ProjectionError::DuplicateCard(id) if id == card_id),
        "wrong error variant: {err:?}",
    );
    // Original card unchanged — silent overwrite would be a bug.
    let card = projection.card(card_id).expect("original card still present");
    assert_eq!(card.title, "original");
    assert_eq!(card.priority, Priority::P1);
    assert_eq!(card.created_by, peer(1));
}
