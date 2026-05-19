//! `airc-rs work ...` handlers.
//!
//! The CLI stays intentionally thin: it parses human input, calls the
//! consumer-facing `airc_lib::Airc` work API, and prints stable lines
//! that other tools can scrape. Work-domain validation and event
//! construction live in `airc-lib` / `airc-work`.

use std::path::Path;

use uuid::Uuid;

use airc_lib::{
    Airc, ClaimId, ClaimWorkCard, CreateWorkCard, LaneId, Priority, ReleaseWorkClaim, RepoId,
    WorkBoardProjection, WorkCardId,
};

use crate::work_cli::CliPriority;

pub async fn run_create(
    home: &Path,
    repo: String,
    title: String,
    body: Option<String>,
    lane_id: Option<String>,
    priority: CliPriority,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new(repo)?,
            title,
            body,
            priority: priority.into(),
            lane_id: parse_optional_lane_id(lane_id.as_deref())?,
        })
        .await?;
    println!("card_id: {card_id}");
    Ok(())
}

pub async fn run_claim(
    home: &Path,
    card_id: String,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let card_id = parse_work_card_id(&card_id)?;
    let claim_id = airc
        .claim_work_card(ClaimWorkCard { card_id, ttl_ms })
        .await?;
    println!("claim_id: {claim_id}");
    Ok(())
}

pub async fn run_release(
    home: &Path,
    card_id: String,
    claim_id: String,
    reason: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    airc.release_work_claim(ReleaseWorkClaim {
        card_id: parse_work_card_id(&card_id)?,
        claim_id: parse_claim_id(&claim_id)?,
        reason,
    })
    .await?;
    println!("released: card_id={card_id} claim_id={claim_id}");
    Ok(())
}

pub async fn run_board(home: &Path, limit: usize) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let board = airc.work_board(limit).await?;
    print_board(&board);
    Ok(())
}

fn print_board(board: &WorkBoardProjection) {
    let snapshot = board.snapshot();
    if snapshot.cards.is_empty() {
        println!("(no work cards)");
        return;
    }

    println!("work cards: {}", snapshot.cards.len());
    for card in &snapshot.cards {
        let owner = card
            .owner
            .map(|peer| peer.to_string())
            .unwrap_or_else(|| "-".to_string());
        let claim = card
            .claim_id
            .map(|claim| claim.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{card_id}  {priority:?}  {state:?}  owner={owner}  claim={claim}  repo={repo}  title={title}",
            card_id = card.card_id,
            priority = card.priority,
            state = card.state,
            repo = card.repo,
            title = card.title,
        );
    }
}

fn parse_work_card_id(input: &str) -> Result<WorkCardId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("work card id {input:?} is not a valid UUID: {error}"))?;
    Ok(WorkCardId::from_uuid(uuid))
}

fn parse_claim_id(input: &str) -> Result<ClaimId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("claim id {input:?} is not a valid UUID: {error}"))?;
    Ok(ClaimId::from_uuid(uuid))
}

fn parse_optional_lane_id(
    input: Option<&str>,
) -> Result<Option<LaneId>, Box<dyn std::error::Error>> {
    input.map(parse_lane_id).transpose()
}

fn parse_lane_id(input: &str) -> Result<LaneId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("lane id {input:?} is not a valid UUID: {error}"))?;
    Ok(LaneId::from_uuid(uuid))
}

impl From<CliPriority> for Priority {
    fn from(value: CliPriority) -> Self {
        match value {
            CliPriority::P0 => Self::P0,
            CliPriority::P1 => Self::P1,
            CliPriority::P2 => Self::P2,
            CliPriority::P3 => Self::P3,
        }
    }
}
