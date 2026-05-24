//! `airc work ...` handlers.
//!
//! The CLI stays intentionally thin: it parses human input, calls the
//! consumer-facing `airc_lib::Airc` work API, and renders terminal
//! output for humans. Integrations should call `airc-lib` / daemon IPC
//! / ORM projections directly rather than parsing CLI output.
//! Work-domain validation and event construction live in `airc-lib` /
//! `airc-work`.

use std::path::Path;

use uuid::Uuid;

use airc_lib::{
    AgentAvailabilityState, Airc, ClaimId, ClaimWorkCard, CreateWorkCard, LaneId, Priority,
    ReleaseWorkClaim, RepoId, WorkBoardProjection, WorkCardId,
};

use crate::work_cli::{CliAvailabilityState, CliPriority};

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

pub async fn run_heartbeat(
    home: &Path,
    card_id: String,
    claim_id: String,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    airc.heartbeat_work_claim(airc_lib::HeartbeatWorkClaim {
        card_id: parse_work_card_id(&card_id)?,
        claim_id: parse_claim_id(&claim_id)?,
        ttl_ms,
    })
    .await?;
    println!("claim_heartbeat: card_id={card_id} claim_id={claim_id} ttl_ms={ttl_ms}");
    Ok(())
}

pub async fn run_board(home: &Path, limit: usize) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let board = airc.work_board(limit).await?;
    print_board(&board);
    Ok(())
}

pub async fn run_next(
    home: &Path,
    repo: Option<String>,
    max_priority: CliPriority,
    include_stale: bool,
    limit: usize,
    event_limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let query = airc_lib::ClaimableWorkQuery {
        repo: repo.map(RepoId::new).transpose()?,
        max_priority: max_priority.into(),
        include_stale_claims: include_stale,
        event_limit,
        limit,
    };
    let items = airc.claimable_work(query).await?;
    if items.is_empty() {
        println!("(no claimable work)");
        return Ok(());
    }

    println!("claimable work: {}", items.len());
    for item in items {
        let stale = item
            .stale_claim
            .as_ref()
            .map(|claim| format!("stale_claim={} owner={}", claim.claim_id, claim.owner))
            .unwrap_or_else(|| "open".to_string());
        println!(
            "{card_id}  {priority:?}  repo={repo}  {stale}  title={title}",
            card_id = item.card.card_id,
            priority = item.card.priority,
            repo = item.card.repo,
            title = item.card.title,
        );
    }
    Ok(())
}

pub async fn run_availability(
    home: &Path,
    repo: String,
    state: CliAvailabilityState,
    note: Option<String>,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let repo = RepoId::new(repo)?;
    airc.report_agent_availability(airc_lib::ReportAgentAvailability {
        repo: repo.clone(),
        state: state.into(),
        note,
        ttl_ms,
    })
    .await?;
    println!("agent_availability: repo={repo} state={state:?} ttl_ms={ttl_ms}");
    Ok(())
}

fn print_board(board: &WorkBoardProjection) {
    let snapshot = board.snapshot();
    if snapshot.cards.is_empty() && snapshot.agent_availability.is_empty() {
        println!("(no work cards)");
        return;
    }
    let stale_claims = board.stale_claims(now_ms());

    if !snapshot.cards.is_empty() {
        println!("work cards: {}", snapshot.cards.len());
    }
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
    if !stale_claims.is_empty() {
        println!();
        println!("stale claims: {}", stale_claims.len());
        for claim in stale_claims {
            println!(
                "{card_id}  owner={owner}  claim={claim_id}  expired_at_ms={expired_at_ms}",
                card_id = claim.card_id,
                owner = claim.owner,
                claim_id = claim.claim_id,
                expired_at_ms = claim.expired_at_ms,
            );
        }
    }
    if !snapshot.agent_availability.is_empty() {
        println!();
        println!("agent availability: {}", snapshot.agent_availability.len());
        for availability in snapshot.agent_availability {
            let stale = availability.expires_at_ms <= now_ms();
            let note = availability.report.note.as_deref().unwrap_or("-");
            println!(
                "{repo}  peer={peer}  state={state:?}  stale={stale}  expires_at_ms={expires_at_ms}  note={note}",
                repo = availability.report.repo,
                peer = availability.report.peer,
                state = availability.report.state,
                expires_at_ms = availability.expires_at_ms,
            );
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
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

impl From<CliAvailabilityState> for AgentAvailabilityState {
    fn from(value: CliAvailabilityState) -> Self {
        match value {
            CliAvailabilityState::Ready => Self::Ready,
            CliAvailabilityState::Busy => Self::Busy,
            CliAvailabilityState::Away => Self::Away,
        }
    }
}
