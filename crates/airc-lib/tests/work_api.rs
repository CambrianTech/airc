use std::time::{Duration, Instant};

use airc_lib::{
    AgentAvailabilityState, Airc, AircError, AllocateWorkspace, BranchName, CardState,
    ChangeWorkCardState, ChangeWorkLaneState, ClaimManagerHat, ClaimWorkCard, ClaimableWorkQuery,
    CreateWorkCard, CreateWorkLane, DirtyState, HeartbeatKind, HeartbeatWorkspace, LaneState,
    ObserveLocalGitWorkspace, Priority, ReleaseManagerHat, ReleaseWorkClaim, ReleaseWorkspace,
    RepoId, ReportAgentAvailability, RequestWorkspace, WorkCardId, WorkRosterQuery,
    WorkspaceStatus,
};
use tempfile::TempDir;

fn git(repo: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    git(repo.path(), &["init", "-b", "main"]);
    git(
        repo.path(),
        &["config", "user.email", "airc@example.invalid"],
    );
    git(repo.path(), &["config", "user.name", "AIRC Test"]);
    std::fs::write(repo.path().join("README.md"), "initial\n").unwrap();
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-m", "initial"]);
    repo
}

async fn wait_for_card(airc: &Airc, card_id: WorkCardId) -> airc_lib::WorkBoardProjection {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let board = airc.work_board(128).await.unwrap();
        if board.card(card_id).is_some() {
            return board;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for work card {card_id}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn create_work_card_publishes_and_projects_from_store() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("work-api").await.unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "wire work api through airc-lib".to_string(),
            body: Some("typed work event over signed substrate".to_string()),
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .unwrap();

    let immediate = airc.work_board(128).await.unwrap();
    assert!(
        immediate.card(card_id).is_some(),
        "own work sends must be immediately visible in the local durable store"
    );

    let board = wait_for_card(&airc, card_id).await;
    let card = board.card(card_id).unwrap();
    assert_eq!(card.title, "wire work api through airc-lib");
    assert_eq!(card.repo.as_str(), "CambrianTech/airc");
    assert_eq!(card.created_by, airc.peer_id());
}

#[tokio::test]
async fn claim_and_release_work_card_round_trip_through_projection() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("work-claims").await.unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "claim via rust api".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
        })
        .await
        .unwrap();
    wait_for_card(&airc, card_id).await;

    let claim_id = airc
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let board = airc.work_board(128).await.unwrap();
        let card = board.card(card_id).unwrap();
        if card.claim_id == Some(claim_id) {
            assert_eq!(card.owner, Some(airc.peer_id()));
            break;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for work claim {claim_id}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    airc.release_work_claim(ReleaseWorkClaim {
        card_id,
        claim_id,
        reason: Some("merged into rust-rewrite".to_string()),
    })
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let board = airc.work_board(128).await.unwrap();
        let card = board.card(card_id).unwrap();
        if card.claim_id.is_none() {
            assert_eq!(card.owner, None);
            break;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for work claim release {claim_id}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn work_card_transitions_refuse_cards_from_another_room() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("room-a").await.unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "room-scoped work".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .unwrap();
    assert!(airc.work_board(128).await.unwrap().card(card_id).is_some());

    airc.join("room-b").await.unwrap();
    let error = airc
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .expect_err("claiming a card from another room must fail closed");

    match error {
        AircError::WorkCardNotInCurrentRoom {
            card_id: rejected,
            room_name,
            ..
        } => {
            assert_eq!(rejected, card_id);
            assert_eq!(room_name, "room-b");
        }
        other => panic!("unexpected error: {other}"),
    }

    airc.change_work_card_state(ChangeWorkCardState {
        card_id,
        state: CardState::Closed,
    })
    .await
    .expect_err("state transitions from another room must fail closed");

    let room_b_board = airc.work_board(128).await.unwrap();
    assert!(
        room_b_board.card(card_id).is_none(),
        "room-b must not project a false state for a room-a card"
    );

    airc.join("room-a").await.unwrap();
    airc.change_work_card_state(ChangeWorkCardState {
        card_id,
        state: CardState::Closed,
    })
    .await
    .unwrap();
    let room_a_board = airc.work_board(128).await.unwrap();
    assert_eq!(room_a_board.card(card_id).unwrap().state, CardState::Closed);
}

#[tokio::test]
async fn duplicate_work_card_claims_fail_before_poisoning_projection() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("duplicate-claim").await.unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "one active owner".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .unwrap();
    let claim_id = airc
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .unwrap();

    let error = airc
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .expect_err("second active claim must fail before emitting a work event");
    match error {
        AircError::WorkCardAlreadyClaimed {
            card_id: rejected,
            claim_id: active_claim,
            owner,
        } => {
            assert_eq!(rejected, card_id);
            assert_eq!(active_claim, Some(claim_id));
            assert_eq!(owner, Some(airc.peer_id()));
        }
        other => panic!("unexpected error: {other}"),
    }

    airc.release_work_claim(ReleaseWorkClaim {
        card_id,
        claim_id,
        reason: Some("done".to_string()),
    })
    .await
    .unwrap();
    let board = airc.work_board(128).await.unwrap();
    assert_eq!(board.card(card_id).unwrap().claim_id, None);
}

#[tokio::test]
async fn claimable_work_suggests_open_priority_cards_without_stdout_parsing() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("claimable-work-api").await.unwrap();
    let repo = RepoId::new("CambrianTech/airc").unwrap();

    let p0 = airc
        .create_work_card(CreateWorkCard {
            repo: repo.clone(),
            title: "take this first".to_string(),
            body: None,
            priority: Priority::P0,
            lane_id: None,
        })
        .await
        .unwrap();
    let p1 = airc
        .create_work_card(CreateWorkCard {
            repo: repo.clone(),
            title: "take this second".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .unwrap();
    let p2 = airc
        .create_work_card(CreateWorkCard {
            repo,
            title: "lower priority".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
        })
        .await
        .unwrap();
    let claimed = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "already claimed".to_string(),
            body: None,
            priority: Priority::P0,
            lane_id: None,
        })
        .await
        .unwrap();
    airc.claim_work_card(ClaimWorkCard {
        card_id: claimed,
        ttl_ms: 60_000,
    })
    .await
    .unwrap();

    let claimable = airc
        .claimable_work(ClaimableWorkQuery {
            event_limit: 128,
            ..ClaimableWorkQuery::default()
        })
        .await
        .unwrap();
    let ids: Vec<WorkCardId> = claimable.iter().map(|item| item.card.card_id).collect();
    assert_eq!(ids, vec![p0, p1]);
    assert!(!ids.contains(&p2));
    assert!(!ids.contains(&claimed));
}

#[tokio::test]
async fn claimable_work_can_surface_stale_claims_for_recovery() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("claimable-stale-api").await.unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "recover stale claim".to_string(),
            body: None,
            priority: Priority::P0,
            lane_id: None,
        })
        .await
        .unwrap();
    let claim_id = airc
        .claim_work_card(ClaimWorkCard { card_id, ttl_ms: 1 })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;

    let hidden = airc
        .claimable_work(ClaimableWorkQuery {
            include_stale_claims: false,
            event_limit: 128,
            ..ClaimableWorkQuery::default()
        })
        .await
        .unwrap();
    assert!(hidden.is_empty());

    let visible = airc
        .claimable_work(ClaimableWorkQuery {
            include_stale_claims: true,
            event_limit: 128,
            ..ClaimableWorkQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].card.card_id, card_id);
    assert_eq!(
        visible[0].stale_claim.as_ref().map(|claim| claim.claim_id),
        Some(claim_id)
    );
}

#[tokio::test]
async fn work_roster_status_combines_liveness_availability_and_claims() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("work-roster-api").await.unwrap();
    let repo = RepoId::new("CambrianTech/airc").unwrap();

    airc.emit_agent_heartbeat(
        HeartbeatKind::Alive,
        "codex",
        Some("airc-worktree".to_string()),
    )
    .await
    .unwrap();
    airc.report_agent_availability(ReportAgentAvailability {
        repo: repo.clone(),
        state: AgentAvailabilityState::Ready,
        note: Some("can take next card".to_string()),
        ttl_ms: 60_000,
    })
    .await
    .unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo,
            title: "render typed work roster".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .unwrap();
    let claim_id = airc
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .unwrap();

    let roster = airc
        .work_roster_status(WorkRosterQuery {
            event_limit: 128,
            ..WorkRosterQuery::default()
        })
        .await
        .unwrap();

    assert_eq!(roster.rows.len(), 1);
    assert_eq!(roster.alive_count(), 1);
    assert_eq!(roster.ready_count(u64::MAX.saturating_sub(1)), 0);
    assert_eq!(roster.ready_count(0), 1);
    let row = &roster.rows[0];
    assert_eq!(row.peer, airc.peer_id());
    assert_eq!(row.liveness.as_ref().unwrap().runtime, "codex");
    assert_eq!(
        row.availability.as_ref().unwrap().report.note.as_deref(),
        Some("can take next card")
    );
    assert_eq!(row.active_claims.len(), 1);
    assert_eq!(row.active_claims[0].card_id, card_id);
    assert_eq!(row.active_claims[0].claim_id, Some(claim_id));
    assert_eq!(roster.claimable_count, 0);
}

#[tokio::test]
async fn work_roster_attaches_peer_claim_to_only_live_client_row() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("work-roster-client-api").await.unwrap();
    let repo = RepoId::new("CambrianTech/airc").unwrap();

    airc.emit_agent_heartbeat_with_metadata(
        HeartbeatKind::Alive,
        "codex",
        Some("codex:one".to_string()),
        Some("airc-worktree".to_string()),
        Some("abc123".to_string()),
    )
    .await
    .unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo,
            title: "correlate claim to live client".to_string(),
            body: None,
            priority: Priority::P0,
            lane_id: None,
        })
        .await
        .unwrap();
    airc.claim_work_card(ClaimWorkCard {
        card_id,
        ttl_ms: 60_000,
    })
    .await
    .unwrap();

    let roster = airc
        .work_roster_status(WorkRosterQuery {
            event_limit: 128,
            ..WorkRosterQuery::default()
        })
        .await
        .unwrap();

    assert_eq!(roster.rows.len(), 1);
    let row = &roster.rows[0];
    assert_eq!(row.peer, airc.peer_id());
    assert_eq!(row.client_id.as_deref(), Some("codex:one"));
    assert_eq!(row.active_claims.len(), 1);
    assert_eq!(row.active_claims[0].card_id, card_id);
    let liveness = row.liveness.as_ref().unwrap();
    assert_eq!(liveness.runtime, "codex");
    assert_eq!(liveness.build.as_deref(), Some("abc123"));
}

#[tokio::test]
async fn work_roster_keeps_peer_claim_separate_when_multiple_clients_are_live() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("work-roster-multi-client-api").await.unwrap();
    let repo = RepoId::new("CambrianTech/airc").unwrap();

    for client in ["codex:one", "codex:two"] {
        airc.emit_agent_heartbeat_with_metadata(
            HeartbeatKind::Alive,
            "codex",
            Some(client.to_string()),
            Some("airc-worktree".to_string()),
            None,
        )
        .await
        .unwrap();
    }

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo,
            title: "ambiguous peer claim".to_string(),
            body: None,
            priority: Priority::P0,
            lane_id: None,
        })
        .await
        .unwrap();
    airc.claim_work_card(ClaimWorkCard {
        card_id,
        ttl_ms: 60_000,
    })
    .await
    .unwrap();

    let roster = airc
        .work_roster_status(WorkRosterQuery {
            event_limit: 128,
            ..WorkRosterQuery::default()
        })
        .await
        .unwrap();

    assert_eq!(roster.rows.len(), 3);
    assert_eq!(
        roster
            .rows
            .iter()
            .filter(|row| row.liveness.is_some())
            .count(),
        2
    );
    let claim_row = roster
        .rows
        .iter()
        .find(|row| !row.active_claims.is_empty())
        .unwrap();
    assert_eq!(claim_row.client_id, None);
    assert_eq!(claim_row.active_claims[0].card_id, card_id);
}

#[tokio::test]
async fn lane_create_attach_card_and_state_change_project_from_store() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("lane-api").await.unwrap();

    let lane_id = airc
        .create_work_lane(CreateWorkLane {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "rust lane surface".to_string(),
            state: LaneState::Planned,
        })
        .await
        .unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "attach card to lane".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: Some(lane_id),
        })
        .await
        .unwrap();

    let board = airc.work_board(128).await.unwrap();
    let snapshot = board.snapshot();
    let lane = snapshot
        .lanes
        .iter()
        .find(|lane| lane.lane_id == lane_id)
        .expect("created lane projects");
    assert_eq!(lane.card_ids, vec![card_id]);
    assert_eq!(lane.state, LaneState::Planned);

    airc.change_work_lane_state(ChangeWorkLaneState {
        lane_id,
        state: LaneState::Active,
    })
    .await
    .unwrap();

    let board = airc.work_board(128).await.unwrap();
    let lane = board
        .snapshot()
        .lanes
        .into_iter()
        .find(|lane| lane.lane_id == lane_id)
        .expect("lane remains projected");
    assert_eq!(lane.state, LaneState::Active);
}

#[tokio::test]
async fn workspace_lifecycle_projects_from_store() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("workspace-api").await.unwrap();

    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "workspace lease lifecycle".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .unwrap();
    let claim_id = airc
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .unwrap();

    let workspace_id = airc
        .request_workspace(RequestWorkspace {
            card_id,
            claim_id,
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            branch: BranchName::new("feat/workspace-commands").unwrap(),
            base: BranchName::new("rust-rewrite").unwrap(),
        })
        .await
        .unwrap();
    airc.allocate_workspace(AllocateWorkspace {
        workspace_id,
        path: "/tmp/airc/ws".to_string(),
    })
    .await
    .unwrap();
    airc.heartbeat_workspace(HeartbeatWorkspace {
        workspace_id,
        disk_bytes: Some(4096),
    })
    .await
    .unwrap();
    airc.release_workspace(ReleaseWorkspace { workspace_id })
        .await
        .unwrap();

    let events = airc.page_recent(128).await.unwrap();
    let lamports: Vec<u64> = events
        .iter()
        .filter(|event| {
            event
                .headers
                .get("forge.body_hint")
                .is_some_and(|hint| hint == "airc.work.event")
        })
        .map(|event| event.lamport)
        .collect();
    assert!(
        lamports.windows(2).all(|pair| pair[0] < pair[1]),
        "work events emitted by one Airc handle must be strictly lamport ordered: {lamports:?}"
    );

    let board = airc.work_board(128).await.unwrap();
    let workspace = board.workspace(workspace_id).unwrap();
    assert_eq!(workspace.lease.status, WorkspaceStatus::Released);
    assert_eq!(workspace.lease.path, "/tmp/airc/ws");
    assert_eq!(workspace.lease.disk_bytes, Some(4096));
    assert_eq!(workspace.lease.card_id, card_id);
    assert_eq!(workspace.lease.claim_id, claim_id);
}

#[tokio::test]
async fn manager_hat_claim_and_release_project_from_store() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("manager-hat-api").await.unwrap();
    let repo = RepoId::new("CambrianTech/airc").unwrap();

    airc.claim_manager_hat(ClaimManagerHat {
        repo: repo.clone(),
        ttl_ms: 60_000,
    })
    .await
    .unwrap();

    let board = airc.work_board(128).await.unwrap();
    let snapshot = board.snapshot();
    let hat = snapshot
        .manager_hats
        .iter()
        .find(|hat| hat.repo == repo)
        .expect("manager hat projects");
    assert_eq!(hat.manager, airc.peer_id());
    assert!(hat.expires_at_ms > hat.claimed_at_ms);

    airc.release_manager_hat(ReleaseManagerHat { repo: repo.clone() })
        .await
        .unwrap();

    let board = airc.work_board(128).await.unwrap();
    assert!(
        board.snapshot().manager_hats.is_empty(),
        "released manager hat must leave projection"
    );
}

#[tokio::test]
async fn local_git_observation_publishes_repo_tracking_events() {
    let home = TempDir::new().unwrap();
    let git_repo = init_git_repo();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("local-git-api").await.unwrap();
    let repo = RepoId::new("CambrianTech/airc").unwrap();

    let observed = airc
        .observe_local_git_workspace(ObserveLocalGitWorkspace {
            repo: repo.clone(),
            workspace_id: None,
            path: git_repo.path().to_path_buf(),
            previous: None,
        })
        .await
        .unwrap();

    assert_eq!(
        observed.emitted_event_ids.len(),
        3,
        "first observation emits commit, branch, and dirty-state records"
    );
    assert_eq!(observed.snapshot.branch.as_str(), "main");
    assert_eq!(observed.snapshot.dirty_state, DirtyState::Clean);

    let board = airc.work_board(128).await.unwrap();
    let tracking = board.repo_tracking(&repo).unwrap();
    assert_eq!(
        tracking
            .branches
            .get(&observed.snapshot.branch)
            .unwrap()
            .head,
        observed.snapshot.head
    );
    assert_eq!(tracking.observed_commits.len(), 1);
    assert_eq!(tracking.dirty_states.len(), 1);

    let unchanged = airc
        .observe_local_git_workspace(ObserveLocalGitWorkspace {
            repo: repo.clone(),
            workspace_id: None,
            path: git_repo.path().to_path_buf(),
            previous: Some(observed.snapshot.clone()),
        })
        .await
        .unwrap();
    assert!(
        unchanged.emitted_event_ids.is_empty(),
        "unchanged git state must not spam the work event stream"
    );

    std::fs::write(git_repo.path().join("README.md"), "changed\n").unwrap();
    let dirty = airc
        .observe_local_git_workspace(ObserveLocalGitWorkspace {
            repo: repo.clone(),
            workspace_id: None,
            path: git_repo.path().to_path_buf(),
            previous: Some(observed.snapshot),
        })
        .await
        .unwrap();

    assert_eq!(dirty.emitted_event_ids.len(), 1);
    assert_eq!(dirty.snapshot.dirty_state, DirtyState::Dirty);
    assert_eq!(dirty.snapshot.dirty_paths, 1);
}
