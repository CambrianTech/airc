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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
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
            reviews: None,
            origin: None,
        }))
        .expect_err("duplicate card_id must surface as an error");
    assert!(
        matches!(err, super::ProjectionError::DuplicateCard(id) if id == card_id),
        "wrong error variant: {err:?}",
    );
    // Original card unchanged — silent overwrite would be a bug.
    let card = projection
        .card(card_id)
        .expect("original card still present");
    assert_eq!(card.title, "original");
    assert_eq!(card.priority, Priority::P1);
    assert_eq!(card.created_by, peer(1));
}

// -------------------------------------------------------------------------
// Card ad7e100b Sub-A — typed `reviews` link on `WorkCard`.
//
// Reviews are a sibling-card relationship: a "review" card carries a
// `reviews = Some(parent_id)` link so observers can ask the projection
// (`review_cards_for(parent_id)`) which reviews exist for a parent
// without scanning bodies. The link must:
//   * project onto `WorkCard.reviews` when the event carries it;
//   * default to `None` when the event omits it (back-compat — legacy
//     events on the wire decode without a `reviews` field);
//   * surface on `review_cards_for(parent_id)`, including the case
//     where multiple sibling reviewers race (every review surfaces,
//     not just the first).
// -------------------------------------------------------------------------

#[test]
fn cards_omitting_reviews_link_project_with_none() {
    let card_id = WorkCardId::from_u128(0xAD7E_100B);
    let mut projection = WorkBoardProjection::new();
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "ordinary card".into(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            created_by: peer(1),
            created_at_ms: 1,
            reviews: None,
            origin: None,
        }))
        .expect("ordinary create projects");

    let card = projection.card(card_id).expect("card present");
    assert_eq!(card.reviews, None);
    assert_eq!(
        projection.review_cards_for(card_id).count(),
        0,
        "no review cards exist for an unreviewed parent"
    );
}

#[test]
fn review_card_links_to_parent_and_surfaces_on_query() {
    let parent_id = WorkCardId::from_u128(0xAD7E_100B);
    let review_id = WorkCardId::from_u128(0xAD7E_F001);
    let mut projection = WorkBoardProjection::new();

    // Parent card: a normal piece of work.
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id: parent_id,
            repo: repo(),
            title: "parent: peer-agent review loop".into(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: peer(1),
            created_at_ms: 1,
            reviews: None,
            origin: None,
        }))
        .expect("parent create projects");

    // Review card: same repo, sibling, typed link back at the parent.
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id: review_id,
            repo: repo(),
            title: "review: peer-agent review loop".into(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: peer(2),
            created_at_ms: 2,
            reviews: Some(parent_id),
            origin: None,
        }))
        .expect("review create projects");

    let review = projection.card(review_id).expect("review present");
    assert_eq!(review.reviews, Some(parent_id));

    let reviews: Vec<_> = projection.review_cards_for(parent_id).collect();
    assert_eq!(reviews.len(), 1, "exactly one review for the parent");
    assert_eq!(reviews[0].card_id, review_id);
    assert_eq!(
        reviews[0].created_by,
        peer(2),
        "review card created_by attribution preserved (reviewer ≠ author)"
    );
}

#[test]
fn multiple_reviewers_each_surface_for_the_same_parent() {
    // Two agents racing to review the same PR each spawn their own
    // review card. The projection MUST surface both: review work is
    // peer-parallel by AGENTS.md §8 (no "lead reviewer" gate).
    let parent_id = WorkCardId::from_u128(0xAD7E_100B);
    let alice_review = WorkCardId::from_u128(0xAD7E_A11C_AAAA);
    let bob_review = WorkCardId::from_u128(0xAD7E_B0B0_BBBB);
    let unrelated = WorkCardId::from_u128(0xCAFE_BABE);

    let mut projection = WorkBoardProjection::new();
    for (id, by, ts, title, reviews) in [
        (parent_id, peer(1), 1u64, "parent", None),
        (alice_review, peer(2), 2, "alice's review", Some(parent_id)),
        (bob_review, peer(3), 3, "bob's review", Some(parent_id)),
        (
            unrelated,
            peer(4),
            4,
            "unrelated card — wrong parent",
            Some(WorkCardId::from_u128(0xDEAD_BEEF)),
        ),
    ] {
        projection
            .apply(&WorkEvent::CardCreated(CardCreated {
                card_id: id,
                repo: repo(),
                title: title.into(),
                body: None,
                priority: Priority::P2,
                lane_id: None,
                created_by: by,
                created_at_ms: ts,
                reviews,
                origin: None,
            }))
            .expect("create projects");
    }

    let mut review_ids: Vec<_> = projection
        .review_cards_for(parent_id)
        .map(|card| card.card_id)
        .collect();
    review_ids.sort_by_key(|id| id.as_uuid());
    let mut expected = vec![alice_review, bob_review];
    expected.sort_by_key(|id| id.as_uuid());
    assert_eq!(
        review_ids, expected,
        "review_cards_for must surface every reviewer, not just the first"
    );

    assert_eq!(
        projection
            .review_cards_for(WorkCardId::from_u128(0xDEAD_BEEF))
            .count(),
        1,
        "review_cards_for filters by parent_id exactly — the 'unrelated' \
         card is only a review of 0xDEAD_BEEF, not of the parent"
    );
}

#[test]
fn reviews_field_round_trips_through_serde_with_back_compat() {
    // Forward direction: an event WITH a reviews link round-trips
    // every field byte-identically, and the typed field is what's
    // recorded on the wire (not stuffed into the body).
    let parent = WorkCardId::new();
    let review_event = CardCreated {
        card_id: WorkCardId::new(),
        repo: repo(),
        title: "review card".into(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        created_by: PeerId::new(),
        created_at_ms: 200,
        reviews: Some(parent),
        origin: None,
    };
    let json = serde_json::to_value(&review_event).expect("encode");
    assert_eq!(
        json["reviews"],
        serde_json::json!(parent.as_uuid().to_string())
    );
    let decoded: CardCreated = serde_json::from_value(json).expect("decode");
    assert_eq!(decoded, review_event);

    // Back-compat: a legacy event (encoded by a peer on pre-ad7e100b
    // code, or a non-review card) carries no `reviews` key on the
    // wire. We synthesize one from a real `CardCreated` with
    // `reviews: None` so the fixture uses typed UUID newtypes
    // throughout (PeerId / WorkCardId / RepoId) — never hand-rolled
    // UUID strings, which can be malformed or collide in p2p.
    let plain = CardCreated {
        reviews: None,
        origin: None,
        ..review_event
    };
    let plain_json = serde_json::to_value(&plain).expect("encode plain");
    assert!(
        plain_json.get("reviews").is_none(),
        "None reviews must not appear in JSON (skip_serializing_if); got {plain_json}"
    );
    let decoded_plain: CardCreated =
        serde_json::from_value(plain_json).expect("legacy-shape decode");
    assert_eq!(
        decoded_plain.reviews, None,
        "missing `reviews` key must deserialize as None, not error"
    );
    assert_eq!(decoded_plain, plain);
}

// -------------------------------------------------------------------------
// Card 5ac0a359 — `CardUpdated` amendment event + projection.
//
// Each editable field is `Option`; `None` = leave alone. `body` is
// double-`Option` so the outer `None` distinguishes "don't touch
// body" from `Some(None)` (clear) and `Some(Some(s))` (set). The
// projection always bumps `updated_at_ms` — even a no-op all-`None`
// amendment moves it (liveness marker without a semantic change).
// Unknown card_id surfaces as `UnknownCard` so an out-of-room
// amendment doesn't silently succeed.
// -------------------------------------------------------------------------

fn project_card_with(
    projection: &mut WorkBoardProjection,
    card_id: WorkCardId,
    title: &str,
    body: Option<&str>,
    priority: Priority,
    created_at_ms: u64,
) {
    projection
        .apply(&WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: title.into(),
            body: body.map(Into::into),
            priority,
            lane_id: None,
            created_by: peer(1),
            created_at_ms,
            reviews: None,
            origin: None,
        }))
        .expect("create projects");
}

#[test]
fn card_updated_title_only_preserves_body_and_priority() {
    let card_id = WorkCardId::new();
    let mut projection = WorkBoardProjection::new();
    project_card_with(
        &mut projection,
        card_id,
        "original",
        Some("original body"),
        Priority::P2,
        100,
    );

    projection
        .apply(&WorkEvent::CardUpdated(CardUpdated {
            card_id,
            title: Some("amended title".into()),
            body: None,
            priority: None,
            updated_by: peer(2),
            updated_at_ms: 200,
        }))
        .expect("title amend projects");

    let card = projection.card(card_id).expect("card present");
    assert_eq!(card.title, "amended title");
    assert_eq!(
        card.body.as_deref(),
        Some("original body"),
        "body untouched when amendment field is None"
    );
    assert_eq!(
        card.priority,
        Priority::P2,
        "priority untouched when amendment field is None"
    );
    assert_eq!(
        card.updated_at_ms, 200,
        "updated_at_ms moves to the amendment's timestamp"
    );
    assert_eq!(
        card.created_at_ms, 100,
        "created_at_ms is append-only (attribution / temporal anchor)"
    );
    assert_eq!(
        card.created_by,
        peer(1),
        "created_by attribution preserved (not the amender)"
    );
}

#[test]
fn card_updated_body_leave_alone_vs_set_vs_empty_string_clear() {
    let card_id = WorkCardId::new();
    let mut projection = WorkBoardProjection::new();
    project_card_with(
        &mut projection,
        card_id,
        "title",
        Some("original body"),
        Priority::P2,
        100,
    );

    // `body: None` — leave alone. Body stays.
    projection
        .apply(&WorkEvent::CardUpdated(CardUpdated {
            card_id,
            title: None,
            body: None,
            priority: None,
            updated_by: peer(2),
            updated_at_ms: 110,
        }))
        .expect("no-op amend projects");
    assert_eq!(
        projection.card(card_id).unwrap().body.as_deref(),
        Some("original body"),
        "None body must NOT touch the existing body"
    );

    // `body: Some("new")` — set to the new value.
    projection
        .apply(&WorkEvent::CardUpdated(CardUpdated {
            card_id,
            title: None,
            body: Some("amended body".into()),
            priority: None,
            updated_by: peer(2),
            updated_at_ms: 120,
        }))
        .expect("set-body amend projects");
    assert_eq!(
        projection.card(card_id).unwrap().body.as_deref(),
        Some("amended body"),
    );

    // `body: Some("")` — the canonical clear path. The projection
    // records exactly what's supplied; renderers treat empty string
    // and None as "no body" identically.
    projection
        .apply(&WorkEvent::CardUpdated(CardUpdated {
            card_id,
            title: None,
            body: Some(String::new()),
            priority: None,
            updated_by: peer(2),
            updated_at_ms: 130,
        }))
        .expect("clear-body amend projects");
    assert_eq!(
        projection.card(card_id).unwrap().body.as_deref(),
        Some(""),
        "Some(\"\") body sets the body to empty (markdown 'no body' idiom)"
    );
}

#[test]
fn card_updated_priority_only_rewrites_priority() {
    let card_id = WorkCardId::new();
    let mut projection = WorkBoardProjection::new();
    project_card_with(&mut projection, card_id, "p2 card", None, Priority::P2, 100);

    projection
        .apply(&WorkEvent::CardUpdated(CardUpdated {
            card_id,
            title: None,
            body: None,
            priority: Some(Priority::P0),
            updated_by: peer(2),
            updated_at_ms: 200,
        }))
        .expect("priority amend projects");

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.priority, Priority::P0);
    assert_eq!(card.title, "p2 card", "title untouched");
}

#[test]
fn card_updated_all_none_bumps_updated_at_only() {
    // The no-op amendment doubles as a liveness marker: an agent
    // "touches" a card without changing semantics. The projection
    // must accept it and bump updated_at_ms.
    let card_id = WorkCardId::new();
    let mut projection = WorkBoardProjection::new();
    project_card_with(
        &mut projection,
        card_id,
        "untouched",
        None,
        Priority::P1,
        100,
    );
    let snapshot_before = projection.card(card_id).cloned().unwrap();

    projection
        .apply(&WorkEvent::CardUpdated(CardUpdated {
            card_id,
            title: None,
            body: None,
            priority: None,
            updated_by: peer(2),
            updated_at_ms: 500,
        }))
        .expect("no-op amend projects");

    let after = projection.card(card_id).unwrap();
    assert_eq!(after.title, snapshot_before.title);
    assert_eq!(after.body, snapshot_before.body);
    assert_eq!(after.priority, snapshot_before.priority);
    assert_eq!(after.state, snapshot_before.state);
    assert_eq!(after.owner, snapshot_before.owner);
    assert_eq!(after.claim_id, snapshot_before.claim_id);
    assert_eq!(after.reviews, snapshot_before.reviews);
    assert_eq!(
        after.updated_at_ms, 500,
        "updated_at_ms moves even on no-op"
    );
}

#[test]
fn card_updated_for_unknown_card_surfaces_error() {
    // An out-of-room amendment would silently succeed if the
    // projection treated missing cards as a no-op — `airc-lib`'s
    // ensure_work_card_in_current_room guard depends on the
    // projection surfacing the error so the typed AircError can be
    // returned.
    let mut projection = WorkBoardProjection::new();
    let err = projection
        .apply(&WorkEvent::CardUpdated(CardUpdated {
            card_id: WorkCardId::new(),
            title: Some("would silently fail".into()),
            body: None,
            priority: None,
            updated_by: peer(2),
            updated_at_ms: 100,
        }))
        .expect_err("amend on missing card must surface");
    assert!(
        matches!(err, super::ProjectionError::UnknownCard(_)),
        "wrong error variant: {err:?}",
    );
}

#[test]
fn card_updated_round_trips_through_serde_with_skip_on_none() {
    // Wire shape contract: every `None` field is omitted via
    // skip_serializing_if so partial amendments stay small on the
    // wire. UUID-typed identity fields are constructed via
    // PeerId::new() / WorkCardId::new() (uuid v4) — no hand-rolled
    // UUID strings in tests.
    let amend = CardUpdated {
        card_id: WorkCardId::new(),
        title: Some("only title".into()),
        body: None,
        priority: None,
        updated_by: PeerId::new(),
        updated_at_ms: 1_700_000_000_000,
    };
    let json = serde_json::to_value(&amend).expect("encode");
    assert!(json.get("title").is_some());
    assert!(
        json.get("body").is_none(),
        "None body must be omitted; got {json}"
    );
    assert!(
        json.get("priority").is_none(),
        "None priority must be omitted; got {json}"
    );
    let decoded: CardUpdated = serde_json::from_value(json).expect("decode");
    assert_eq!(decoded, amend);

    // Clear-body shape: Some("") — empty string round-trips
    // cleanly (unlike Option<Option<String>>'s Some(None), which
    // collapses to None on serde's default decode path). This is
    // the canonical "clear" idiom — observers that render the body
    // treat empty string and None identically (no body).
    let clear = CardUpdated {
        card_id: WorkCardId::new(),
        title: None,
        body: Some(String::new()),
        priority: None,
        updated_by: PeerId::new(),
        updated_at_ms: 1_700_000_000_001,
    };
    let json = serde_json::to_value(&clear).expect("encode clear");
    assert_eq!(
        json.get("body"),
        Some(&serde_json::Value::String(String::new())),
        "Some(\"\") body must appear as the empty string on the wire; got {json}"
    );
    let decoded: CardUpdated = serde_json::from_value(json).expect("decode clear");
    assert_eq!(decoded, clear);
}

// =====================================================================
// Card 1291173d — projection rebuild perf benchmarks
//
// Methodology mirrors card 512fd8a1's header benches: write the
// measurement FIRST, run, identify the actual bottleneck.
// =====================================================================

/// Build a realistic event log: M cards × K mutations each (claim,
/// heartbeat, state-change). Models a multi-day room: ~50 cards
/// active, each with ~5 mutation events = ~250 events. The
/// merger calls work_board(256) which replays this whole log on
/// every tick.
fn build_realistic_event_log(n_cards: u32, mutations_per_card: u32) -> Vec<WorkEvent> {
    let mut events = Vec::with_capacity((n_cards * (1 + mutations_per_card)) as usize);
    for i in 0..n_cards {
        let card_id = WorkCardId::from_u128(u128::from(i) + 1);
        let claim_id = ClaimId::from_u128(u128::from(i) + 1_000_000);
        let owner = peer(u128::from(i) + 1);
        events.push(WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: format!("perf card #{i}"),
            body: Some(format!(
                "synthetic body for card {i} — realistic mid-length \
                 body string capturing typical card prose"
            )),
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 1_000 + u64::from(i),
            reviews: None,
            origin: None,
        }));
        events.push(WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id,
            owner,
            ttl_ms: 600_000,
            claimed_at_ms: 2_000 + u64::from(i),
        }));
        for h in 0..mutations_per_card {
            events.push(WorkEvent::ClaimHeartbeat(ClaimHeartbeat {
                card_id,
                claim_id,
                owner,
                ttl_ms: 600_000,
                heartbeat_at_ms: 3_000 + u64::from(i) + u64::from(h),
            }));
        }
    }
    events
}

#[test]
fn bench_projection_apply_throughput() {
    // How fast can `apply()` consume events? Dominant cost path for
    // any rebuild or replay; we should be able to apply 10k events
    // in well under a millisecond.
    let events = build_realistic_event_log(50, 5); // 50 × (1 created + 1 claimed + 5 heartbeats) = 350 events

    // Warm.
    let mut warm = WorkBoardProjection::new();
    for e in &events {
        warm.apply(e).unwrap();
    }

    const ITERS: u64 = 100;
    let start = std::time::Instant::now();
    for _ in 0..ITERS {
        let mut p = WorkBoardProjection::new();
        for e in &events {
            p.apply(e).unwrap();
        }
        // Read the projection so the optimiser can't elide.
        let _ = p.cards.len();
    }
    let elapsed = start.elapsed();
    let total_events = ITERS * events.len() as u64;
    let ns_per_event = elapsed.as_nanos() as u64 / total_events;
    eprintln!(
        "card 1291173d: projection.apply throughput — {} events × {ITERS} rebuilds in {elapsed:?}, \
         {ns_per_event} ns/event, total {total_events} applies",
        events.len()
    );

    // Floor: applying a single event should never cost more than
    // 10μs. The realistic mix here measures ~hundreds of ns on M2.
    assert!(
        ns_per_event < 10_000,
        "projection.apply regressed to {ns_per_event} ns/event"
    );
}

#[test]
fn bench_projection_replay_realistic_log() {
    // The actual hot-path shape: rebuild the WHOLE projection from
    // event-log replay. Equivalent to what
    // SqliteEventStore::page_recent → Airc::work_board does on
    // every tick of the merger and every `airc work board` call.
    let small = build_realistic_event_log(10, 3); // ~50 events
    let medium = build_realistic_event_log(50, 5); // ~350 events
    let large = build_realistic_event_log(200, 5); // ~1400 events

    for (label, events) in [("50ev", &small), ("350ev", &medium), ("1.4kev", &large)] {
        // Warm.
        let _ = WorkBoardProjection::replay(events.iter().cloned()).unwrap();

        const ITERS: u64 = 50;
        let start = std::time::Instant::now();
        let mut sink = 0usize;
        for _ in 0..ITERS {
            let p = WorkBoardProjection::replay(events.iter().cloned()).unwrap();
            sink = sink.wrapping_add(p.cards.len());
        }
        let elapsed = start.elapsed();
        let ns_per_rebuild = elapsed.as_nanos() as u64 / ITERS;
        let ns_per_event = ns_per_rebuild / events.len() as u64;
        eprintln!(
            "card 1291173d: projection.replay [{label}] — {ITERS} full rebuilds of {} events in {elapsed:?}, \
             {ns_per_rebuild} ns/rebuild, {ns_per_event} ns/event, sink={sink}",
            events.len()
        );

        // A full rebuild of 1.4k events should stay under 5ms; merger
        // ticks every 60s so the latency budget is generous, but the
        // CLI `airc work board` user-facing call wants to feel
        // instant (< 50ms total round-trip including DB read).
        assert!(
            ns_per_rebuild < 50_000_000,
            "projection.replay [{label}] regressed to {ns_per_rebuild} ns/rebuild — \
             this lands on every `airc work board` and every merger tick"
        );
    }
}

#[test]
fn bench_projection_snapshot_clone_cost() {
    // `snapshot()` clones every internal HashMap's values into a Vec.
    // For 200 cards each carrying String fields, the per-snapshot
    // cost is dominated by String::clone. If this is large, the
    // upcoming streaming-snapshot work (borrowed view of the
    // projection state) is justified.
    let events = build_realistic_event_log(200, 0); // 200 cards, no mutations
    let projection = WorkBoardProjection::replay(events.iter().cloned()).unwrap();

    // Warm.
    for _ in 0..100 {
        let _ = projection.snapshot();
    }

    const ITERS: u64 = 1_000;
    let start = std::time::Instant::now();
    let mut sink = 0usize;
    for _ in 0..ITERS {
        let snap = projection.snapshot();
        sink = sink.wrapping_add(snap.cards.len());
    }
    let elapsed = start.elapsed();
    let ns_per_snapshot = elapsed.as_nanos() as u64 / ITERS;
    eprintln!(
        "card 1291173d: projection.snapshot — {ITERS} snapshots of {}-card projection in {elapsed:?}, \
         {ns_per_snapshot} ns/snapshot, sink={sink}",
        projection.cards.len()
    );

    // Floor: a snapshot of a 200-card projection should be < 1ms.
    assert!(
        ns_per_snapshot < 1_000_000,
        "projection.snapshot regressed to {ns_per_snapshot} ns/snapshot"
    );
}

// Card 1291173d: `apply_windowed` is the one apply rule shared by
// `replay_window` and incremental resume from a cached snapshot —
// missing-anchor events are skipped, structural errors stay loud.
#[test]
fn apply_windowed_skips_missing_anchor_and_fails_structural_errors() {
    let card_id = WorkCardId::from_u128(1);
    let mut projection = WorkBoardProjection::new();

    // Anchor missing: the card was created before this window /
    // snapshot — skip, exactly like replay_window.
    projection
        .apply_windowed(&WorkEvent::CardStateChanged(CardStateChanged {
            card_id,
            state: CardState::Review,
            changed_by: peer(3),
            changed_at_ms: 100,
        }))
        .unwrap();
    assert!(projection.card(card_id).is_none());

    let created = WorkEvent::CardCreated(CardCreated {
        card_id,
        repo: repo(),
        title: "windowed apply".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        created_by: peer(3),
        created_at_ms: 100,
        reviews: None,
        origin: None,
    });
    projection.apply_windowed(&created).unwrap();
    assert!(projection.card(card_id).is_some());

    // Structural error (duplicate create) is NOT window tolerance —
    // it must stay loud on both the replay and the resume paths.
    assert_eq!(
        projection.apply_windowed(&created),
        Err(ProjectionError::DuplicateCard(card_id))
    );
}
