//! `airc work ...` handlers.
//!
//! The CLI stays intentionally thin: it parses human input, calls the
//! consumer-facing `airc_lib::Airc` work API, and renders terminal
//! output for humans. Integrations should call `airc-lib` / daemon IPC
//! / ORM projections directly rather than parsing CLI output.
//! Work-domain validation and event construction live in `airc-lib` /
//! `airc-work`.

use std::path::Path;

use airc_diagnostics::{DiagnosticCode, DiagnosticComponent, DiagnosticEvent};
use uuid::Uuid;

use airc_lib::{
    AgentAvailabilityState, Airc, CardState, ChangeWorkCardState, ClaimId, ClaimWorkCard,
    CreateWorkCard, LaneId, Priority, ReleaseWorkClaim, RepoId, WorkBoardProjection, WorkCardId,
    WorkQueueStatus, WorkRosterStatus,
};

use crate::lease;
use crate::work_cli::{CliAvailabilityState, CliCardState, CliPriority};

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
    no_lease_required: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !no_lease_required {
        let check = lease::check_current_dir()?;
        if !check.under_lease {
            return Err(format!(
                "refusing to claim work card from {cwd}: not under lease zone {root}.\n\
                 Allocate a worktree under ~/.airc/worktrees/ first, or pass \
                 --no-lease-required to override.",
                cwd = check.path.display(),
                root = check.lease_root.display(),
            )
            .into());
        }
    }
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
    let card_uuid = parse_work_card_id(&card_id)?;
    let claim_uuid = parse_claim_id(&claim_id)?;
    airc.heartbeat_work_claim(airc_lib::HeartbeatWorkClaim {
        card_id: card_uuid,
        claim_id: claim_uuid,
        ttl_ms,
    })
    .await?;
    println!("claim_heartbeat: card_id={card_id} claim_id={claim_id} ttl_ms={ttl_ms}");

    // Heartbeat doesn't refuse on lease drift — the claim was
    // already granted, and refusing here would just orphan it. But
    // we DO record the drift as a typed diagnostic so a substrate
    // observer can see when an agent has wandered out of its lease.
    if let Ok(check) = lease::check_current_dir() {
        if !check.under_lease {
            let diag = DiagnosticEvent::warn(
                DiagnosticComponent::Work,
                DiagnosticCode::WorkspaceLeaseViolation,
                "work heartbeat fired from a path outside the lease zone",
            )
            .with_field("card_id", &card_id)
            .with_field("claim_id", &claim_id)
            .with_field("cwd", check.path.display())
            .with_field("lease_root", check.lease_root.display());
            // Best-effort: a publish failure shouldn't break the
            // heartbeat from the user's perspective. Surface to
            // stderr so it isn't silently swallowed.
            if let Err(error) = airc.publish_diagnostic_event(&diag).await {
                eprintln!("airc: failed to publish lease-violation diagnostic: {error}");
            }
        }
    }
    Ok(())
}

pub async fn run_state(
    home: &Path,
    card_id: String,
    state: CliCardState,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let card_id = parse_work_card_id(&card_id)?;
    let state = CardState::from(state);
    airc.change_work_card_state(ChangeWorkCardState { card_id, state })
        .await?;
    println!("card_state_changed: card_id={card_id} state={state:?}");
    Ok(())
}

pub async fn run_close(home: &Path, card_id: String) -> Result<(), Box<dyn std::error::Error>> {
    run_state(home, card_id, CliCardState::Closed).await
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
    let query = airc_lib::WorkQueueStatusQuery {
        repo: repo.map(RepoId::new).transpose()?,
        max_priority: max_priority.into(),
        include_stale_claims: include_stale,
        event_limit,
        limit,
    };
    let status = airc.work_queue_status(query).await?;
    if status.claimable.is_empty() {
        println!("(no claimable work)");
    } else {
        println!("claimable work: {}", status.claimable.len());
        for item in &status.claimable {
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
    }

    print_work_queue_availability(&status);
    if !status.active_claims_for_peer.is_empty() {
        println!();
        println!(
            "your active claims: {}",
            status.active_claims_for_peer.len()
        );
        for card in &status.active_claims_for_peer {
            println!(
                "{card_id}  {priority:?}  repo={repo}  title={title}",
                card_id = card.card_id,
                priority = card.priority,
                repo = card.repo,
                title = card.title,
            );
        }
    }
    Ok(())
}

pub async fn run_roster(
    home: &Path,
    repo: Option<String>,
    event_limit: usize,
    active_within_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let status = airc
        .work_roster_status(airc_lib::WorkRosterQuery {
            repo: repo.map(RepoId::new).transpose()?,
            event_limit,
            active_within_ms,
        })
        .await?;
    print_work_roster(&status);
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

fn print_work_queue_availability(status: &WorkQueueStatus) {
    if status.agent_availability.is_empty() {
        return;
    }

    let now_ms = now_ms();
    println!();
    println!(
        "agent availability: ready={} busy={} away={} stale={}",
        status.ready_count(now_ms),
        status.busy_count(now_ms),
        status.away_count(now_ms),
        status.stale_availability_count(now_ms)
    );
    for availability in &status.agent_availability {
        let stale = availability.expires_at_ms <= now_ms;
        let note = availability.report.note.as_deref().unwrap_or("-");
        println!(
            "{repo}  peer={peer}  state={state:?}  stale={stale}  note={note}",
            repo = availability.report.repo,
            peer = availability.report.peer,
            state = availability.report.state,
        );
    }
}

fn print_work_roster(status: &WorkRosterStatus) {
    let now_ms = now_ms();
    if status.rows.is_empty() {
        println!("work roster: 0 agent(s)");
        println!("claimable work: {}", status.claimable_count);
        return;
    }

    println!(
        "work roster: {} agent(s) live={} ready={} busy={} away={} stale_availability={} claimable={}",
        status.rows.len(),
        status.alive_count(),
        status.ready_count(now_ms),
        status.busy_count(now_ms),
        status.away_count(now_ms),
        status.stale_availability_count(now_ms),
        status.claimable_count
    );
    for row in &status.rows {
        let live = row
            .liveness
            .as_ref()
            .map(|liveness| {
                format!(
                    "live runtime={} scope={} last_seen_ms={}",
                    liveness.runtime,
                    liveness.scope.as_deref().unwrap_or("-"),
                    liveness.last_seen_ms
                )
            })
            .unwrap_or_else(|| "live=false".to_string());
        let availability = row
            .availability
            .as_ref()
            .map(|availability| {
                format!(
                    "availability={:?} repo={} stale={} note={}",
                    availability.report.state,
                    availability.report.repo,
                    availability.expires_at_ms <= now_ms,
                    availability.report.note.as_deref().unwrap_or("-")
                )
            })
            .unwrap_or_else(|| "availability=unknown".to_string());
        println!(
            "peer={peer}  {live}  {availability}  claims={claims}",
            peer = row.peer,
            claims = row.active_claims.len(),
        );
        for card in &row.active_claims {
            println!(
                "  claim {card_id}  {priority:?}  {state:?}  repo={repo}  title={title}",
                card_id = card.card_id,
                priority = card.priority,
                state = card.state,
                repo = card.repo,
                title = card.title,
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

impl From<CliCardState> for CardState {
    fn from(value: CliCardState) -> Self {
        match value {
            CliCardState::Open => Self::Open,
            CliCardState::Claimed => Self::Claimed,
            CliCardState::InProgress => Self::InProgress,
            CliCardState::Blocked => Self::Blocked,
            CliCardState::Review => Self::Review,
            CliCardState::Merged => Self::Merged,
            CliCardState::Closed => Self::Closed,
        }
    }
}
