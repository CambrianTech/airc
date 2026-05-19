use std::time::{Duration, Instant};

use airc_lib::{
    Airc, AllocateWorkspace, BranchName, ChangeWorkLaneState, ClaimWorkCard, CreateWorkCard,
    CreateWorkLane, HeartbeatWorkspace, LaneState, Priority, ReleaseWorkClaim, ReleaseWorkspace,
    RepoId, RequestWorkspace, WorkCardId, WorkspaceStatus,
};
use tempfile::TempDir;

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

    let board = airc.work_board(128).await.unwrap();
    let workspace = board.workspace(workspace_id).unwrap();
    assert_eq!(workspace.lease.status, WorkspaceStatus::Released);
    assert_eq!(workspace.lease.path, "/tmp/airc/ws");
    assert_eq!(workspace.lease.disk_bytes, Some(4096));
    assert_eq!(workspace.lease.card_id, card_id);
    assert_eq!(workspace.lease.claim_id, claim_id);
}
