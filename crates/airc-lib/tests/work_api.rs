use std::time::{Duration, Instant};

use airc_lib::{
    Airc, ClaimWorkCard, CreateWorkCard, Priority, ReleaseWorkClaim, RepoId, WorkCardId,
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
