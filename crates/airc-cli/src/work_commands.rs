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
    AgentAvailabilityState, CardState, ChangeWorkCardState, ClaimId, ClaimWorkCard,
    CreateWorkCard, LaneId, Priority, ReleaseWorkClaim, RepoId, WorkBacklogSeedCandidate,
    WorkBacklogSeedOutcome, WorkBoardProjection, WorkCardId, WorkManagerRecommendation,
    WorkManagerRecommendationKind, WorkManagerStatus, WorkQueueStatus, WorkRosterStatus,
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
    let airc = crate::commands::attached_airc(home).await?;
    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new(repo)?,
            title,
            body,
            priority: priority.into(),
            lane_id: parse_optional_lane_id(lane_id.as_deref())?,
            reviews: None,
        })
        .await?;
    println!("card_id: {card_id}");
    Ok(())
}

pub async fn run_seed(
    home: &Path,
    repo: String,
    title: String,
    body: Option<String>,
    lane_id: Option<String>,
    priority: CliPriority,
    evidence_key: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let result = airc
        .seed_work_backlog(vec![WorkBacklogSeedCandidate {
            repo: RepoId::new(repo)?,
            title,
            body,
            priority: priority.into(),
            lane_id: parse_optional_lane_id(lane_id.as_deref())?,
            evidence_key,
        }])
        .await?;
    for item in result.items {
        let outcome = match item.outcome {
            WorkBacklogSeedOutcome::Created => "created",
            WorkBacklogSeedOutcome::AlreadyRepresented => "already_represented",
            WorkBacklogSeedOutcome::AlreadyCompleted => "already_completed",
        };
        println!(
            "seeded: outcome={outcome} card_id={card_id} repo={repo} title={title}",
            card_id = item.card_id,
            repo = item.candidate.repo,
            title = item.candidate.title,
        );
    }
    Ok(())
}

/// `airc work review <PARENT> [--pr URL] [--priority P] [--body B]` —
/// spawn a sibling review card with a typed `reviews` link back to
/// the parent. Card ad7e100b Sub-B (the CLI half of the peer-agent
/// review loop); Sub-A shipped the typed substrate.
///
/// Lookup rules:
///   * Parent must exist in the current room's board projection. The
///     review card is created in the SAME repo as the parent (cross-
///     repo reviews aren't a thing — reviews live where the work
///     lives).
///   * Priority defaults to the parent's priority. The review of a
///     P0 is P0-eligible work; the default keeps that visible
///     without the caller having to spell it out.
///   * Title is generated as `review: <parent.title>` (truncated at
///     a reasonable bound to stay board-renderable).
///   * Body prepends the parent card id and any `--pr` URL so a
///     reviewer who pulls just this card has the navigation anchors;
///     `--body` content (if any) follows.
pub async fn run_review(
    home: &Path,
    parent_id: String,
    pr: Option<String>,
    priority: Option<CliPriority>,
    body: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent_card_id = parse_work_card_id(&parent_id)?;
    let airc = crate::commands::attached_airc(home).await?;

    // Resolve the parent off the current room's board. Refusing on
    // "no parent" is more useful than spawning an orphan review.
    let board = airc.work_board(usize::MAX).await?;
    let parent = board.card(parent_card_id).ok_or_else(|| {
        format!(
            "parent card {parent_card_id} not found in the current room's board; \
             switch to the room that owns it, or pass the correct id"
        )
    })?;

    // Title — same convention an auto-spawn (Sub-C) would use, so
    // manual and auto-created review cards render identically.
    let title = format_review_title(&parent.title);

    // Body — the navigation anchors a reviewer needs, then the
    // caller's prose. The parent id is a UUID so reviewers can
    // `airc work board` and find the parent fast; `--pr` URL gives
    // them the diff directly.
    let mut body_buf = String::new();
    body_buf.push_str(&format!("review of card {parent_card_id}"));
    if let Some(ref url) = pr {
        body_buf.push_str("\nPR: ");
        body_buf.push_str(url);
    }
    if let Some(extra) = body {
        body_buf.push_str("\n\n");
        body_buf.push_str(&extra);
    }

    // Priority — inherits the parent's unless overridden. Reviews
    // of high-priority work are themselves high-priority.
    let final_priority = priority
        .map(Into::into)
        .unwrap_or(parent.priority);

    // Construct via the airc-lib request type with the typed link
    // populated. Sub-A added `.reviewing(parent)` precisely so this
    // call doesn't have to spell out `reviews: Some(parent)` inline.
    let request = CreateWorkCard::new(parent.repo.clone(), title, final_priority)
        .reviewing(parent_card_id);
    let request = CreateWorkCard {
        body: Some(body_buf),
        ..request
    };

    let review_card_id = airc.create_work_card(request).await?;
    println!("review_card_id: {review_card_id} parent_card_id: {parent_card_id}");
    Ok(())
}

/// Generate the review card's title from the parent's. Format is
/// stable so Sub-C (auto-spawn) and Sub-B (CLI) produce identical
/// titles — observers that filter on `title.starts_with("review:")`
/// pick up both paths.
fn format_review_title(parent_title: &str) -> String {
    // 80-char body keeps board rendering tidy without aggressively
    // truncating informative parent titles.
    const MAX_PARENT_LEN: usize = 80;
    let parent_short: String = parent_title.chars().take(MAX_PARENT_LEN).collect();
    if parent_short.chars().count() < parent_title.chars().count() {
        format!("review: {parent_short}…")
    } else {
        format!("review: {parent_short}")
    }
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
    let airc = crate::commands::attached_airc(home).await?;
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
    claim_id: Option<String>,
    reason: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;
    // Default: resolve THIS peer's active claim from the board so
    // callers don't have to track claim_ids the system already knows
    // (kink card acb8bfcd: release ergonomics).
    let claim_uuid = match claim_id {
        Some(raw) => parse_claim_id(&raw)?,
        None => resolve_my_active_claim(&airc, card_uuid).await?,
    };
    airc.release_work_claim(ReleaseWorkClaim {
        card_id: card_uuid,
        claim_id: claim_uuid,
        reason,
    })
    .await?;
    println!("released: card_id={card_id} claim_id={claim_uuid}");
    Ok(())
}

/// Look up the active claim on `card_id` held by this peer via the
/// board projection. Surfaces clear errors when there is no claim, or
/// the claim is held by another peer — so the default never silently
/// releases someone else's work.
async fn resolve_my_active_claim(
    airc: &airc_lib::Airc,
    card_id: airc_lib::WorkCardId,
) -> Result<airc_lib::ClaimId, Box<dyn std::error::Error>> {
    let board = airc.work_board(usize::MAX).await?;
    let card = board
        .card(card_id)
        .ok_or_else(|| format!("card {card_id} not present in the board projection"))?;
    let me = airc.peer_id();
    match (card.owner, card.claim_id) {
        (Some(owner), Some(claim_id)) if owner == me => Ok(claim_id),
        (Some(owner), _) => Err(format!(
            "card {card_id} is currently claimed by {owner}, not this peer ({me}); \
             pass CLAIM_ID explicitly to release a claim you don't own"
        )
        .into()),
        (None, _) => Err(format!("card {card_id} has no active claim to release").into()),
    }
}

pub async fn run_heartbeat(
    home: &Path,
    card_id: String,
    claim_id: String,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
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
    let airc = crate::commands::attached_airc(home).await?;
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

/// First 8 chars of a UUID-style id — enough to disambiguate at the
/// board's typical scale, much easier on the eye than 36-char UUIDs.
/// We deliberately do NOT shorten card_id in the board output because
/// callers copy-paste it into `claim` / `state` / `close`; it's the
/// API key, not a display field.
fn short_id<T: std::fmt::Display>(id: T) -> String {
    id.to_string().chars().take(8).collect()
}

/// Render a peer-id for the board: 'me' for self, the published alias
/// when known (kink 6f111211 / card c397567a — looked up via
/// Airc::peer_alias and pre-fetched into the map by run_board), else
/// short-uuid fallback. Honest "unknown" rendering keeps the board
/// readable even when a peer hasn't published an identity card to
/// this room yet.
fn format_peer(
    peer: airc_lib::PeerId,
    me: airc_lib::PeerId,
    aliases: &std::collections::HashMap<airc_lib::PeerId, String>,
) -> String {
    if peer == me {
        "me".to_string()
    } else if let Some(alias) = aliases.get(&peer) {
        alias.clone()
    } else {
        short_id(peer)
    }
}

/// Filter for `airc work board` output — clap enforces mutual exclusion
/// on the underlying flags, but the runtime collapses them into one
/// value so the rest of the code doesn't carry three booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardFilter {
    All,
    /// No active claim, or claim's lease has expired (eligible for
    /// reclaim per the flywheel-continuity doctrine). Closed/Merged
    /// terminal cards are excluded.
    Available,
    /// Owned by this peer right now.
    Mine,
    /// Owned by another peer (active claim).
    Others,
}

impl BoardFilter {
    pub fn from_flags(available: bool, mine: bool, others: bool) -> Self {
        match (available, mine, others) {
            (true, _, _) => Self::Available,
            (_, true, _) => Self::Mine,
            (_, _, true) => Self::Others,
            _ => Self::All,
        }
    }

    fn matches(self, card: &airc_work::WorkCard, me: airc_lib::PeerId, now_ms: u64) -> bool {
        match self {
            Self::All => true,
            Self::Available => {
                use airc_work::model::CardState;
                if matches!(card.state, CardState::Closed | CardState::Merged) {
                    return false;
                }
                match (card.claim_id, card.claim_expires_at_ms) {
                    (None, _) => true,
                    (Some(_), Some(exp)) if exp <= now_ms => true,
                    _ => false,
                }
            }
            Self::Mine => card.owner == Some(me),
            Self::Others => card.owner.is_some_and(|owner| owner != me),
        }
    }
}

/// Render claim-lease liveness inline on the board (kink ac6affc7):
/// '-' = no claim, '<STALE>' = lease expired (eligible for reclaim per
/// the flywheel-continuity doctrine), otherwise time remaining as
/// 'Mm SS s'. Lets at-a-glance scanning of the board surface stale
/// claims without having to cross-reference 'work roster' + ttl.
fn format_lease(expires_at_ms: Option<u64>, now_ms: u64) -> String {
    match expires_at_ms {
        None => "-".to_string(),
        Some(expires_at_ms) if expires_at_ms <= now_ms => "<STALE>".to_string(),
        Some(expires_at_ms) => {
            let ms_left = expires_at_ms - now_ms;
            let s_left = ms_left / 1000;
            let m = s_left / 60;
            let s = s_left % 60;
            format!("{m}m{s:02}s")
        }
    }
}

pub async fn run_board(
    home: &Path,
    limit: usize,
    filter: BoardFilter,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let board = airc.work_board(limit).await?;
    let me = airc.peer_id();

    // Pre-fetch published aliases for every distinct non-self owner on
    // the board (kink 6f111211 / card c397567a). One scan per peer for
    // now — N is small, and peer_alias is page_recent-backed; if this
    // becomes a hot path the follow-up is one shared scan + a
    // local index.
    let snapshot = board.snapshot();
    let now = now_ms();
    let mut owner_peers: std::collections::HashSet<airc_lib::PeerId> =
        std::collections::HashSet::new();
    for card in &snapshot.cards {
        if let Some(owner) = card.owner {
            if owner != me {
                owner_peers.insert(owner);
            }
        }
    }
    for claim in board.stale_claims(now) {
        if claim.owner != me {
            owner_peers.insert(claim.owner);
        }
    }
    let mut aliases: std::collections::HashMap<airc_lib::PeerId, String> =
        std::collections::HashMap::new();
    for peer in owner_peers {
        if let Ok(Some(alias)) = airc.peer_alias(peer).await {
            aliases.insert(peer, alias);
        }
    }

    print_board(&board, me, filter, &aliases);
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
    let airc = crate::commands::attached_airc(home).await?;
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
    let airc = crate::commands::attached_airc(home).await?;
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

pub async fn run_manage(
    home: &Path,
    repo: Option<String>,
    max_priority: CliPriority,
    include_stale: bool,
    limit: usize,
    event_limit: usize,
    active_within_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let status = airc
        .work_manager_status(airc_lib::WorkManagerQuery {
            repo: repo.map(RepoId::new).transpose()?,
            max_priority: max_priority.into(),
            include_stale_claims: include_stale,
            event_limit,
            limit,
            active_within_ms,
        })
        .await?;
    print_work_manager(&status);
    Ok(())
}

pub async fn run_availability(
    home: &Path,
    repo: String,
    state: CliAvailabilityState,
    note: Option<String>,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
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

fn print_board(
    board: &WorkBoardProjection,
    me: airc_lib::PeerId,
    filter: BoardFilter,
    aliases: &std::collections::HashMap<airc_lib::PeerId, String>,
) {
    let snapshot = board.snapshot();
    if snapshot.cards.is_empty() && snapshot.agent_availability.is_empty() {
        println!("(no work cards)");
        return;
    }
    let stale_claims = board.stale_claims(now_ms());

    let now = now_ms();
    let visible: Vec<&airc_work::WorkCard> = snapshot
        .cards
        .iter()
        .filter(|card| filter.matches(card, me, now))
        .collect();
    if !visible.is_empty() {
        if matches!(filter, BoardFilter::All) {
            println!("work cards: {}", visible.len());
        } else {
            println!(
                "work cards: {} (filter={:?}, hidden={})",
                visible.len(),
                filter,
                snapshot.cards.len() - visible.len(),
            );
        }
    } else if !matches!(filter, BoardFilter::All) {
        println!("(no cards match filter {:?})", filter);
    }
    for card in &visible {
        let owner = card
            .owner
            .map(|peer| format_peer(peer, me, aliases))
            .unwrap_or_else(|| "-".to_string());
        let claim = card
            .claim_id
            .map(short_id)
            .unwrap_or_else(|| "-".to_string());
        let lease = format_lease(card.claim_expires_at_ms, now);
        println!(
            "{card_id}  {priority:?}  {state:?}  owner={owner}  claim={claim}  lease={lease}  repo={repo}  title={title}",
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
                owner = format_peer(claim.owner, me, aliases),
                claim_id = short_id(claim.claim_id),
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
                let client = liveness.client_id.as_deref().unwrap_or("-");
                let build = liveness.build.as_deref().unwrap_or("-");
                format!(
                    "live runtime={} client={} scope={} build={} last_seen_ms={}",
                    liveness.runtime,
                    client,
                    liveness.scope.as_deref().unwrap_or("-"),
                    build,
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

fn print_work_manager(status: &WorkManagerStatus) {
    let now_ms = now_ms();
    println!(
        "work manager: recommendations={} claimable={} agents={} live={} ready={} busy={} away={}",
        status.recommendations.len(),
        status.queue.claimable.len(),
        status.roster.rows.len(),
        status.roster.alive_count(),
        status.roster.ready_count(now_ms),
        status.roster.busy_count(now_ms),
        status.roster.away_count(now_ms),
    );
    if status.recommendations.is_empty() {
        println!("action: none");
        return;
    }
    for recommendation in &status.recommendations {
        print_manager_recommendation(recommendation);
    }
}

fn print_manager_recommendation(recommendation: &WorkManagerRecommendation) {
    let action = match recommendation.kind {
        WorkManagerRecommendationKind::ClaimWork => "claim-work",
        WorkManagerRecommendationKind::RecoverStaleClaim => "recover-stale-claim",
        WorkManagerRecommendationKind::PublishAvailability => "publish-availability",
        WorkManagerRecommendationKind::SeedBacklog => "seed-backlog",
        WorkManagerRecommendationKind::Wait => "wait",
    };
    let card = recommendation
        .card
        .as_ref()
        .map(|card| {
            format!(
                " card={} priority={:?} repo={} title={}",
                card.card_id, card.priority, card.repo, card.title
            )
        })
        .unwrap_or_default();
    let stale = recommendation
        .stale_claim
        .as_ref()
        .map(|claim| {
            format!(
                " stale_claim={} stale_owner={}",
                claim.claim_id, claim.owner
            )
        })
        .unwrap_or_default();
    let agent = recommendation
        .agent
        .as_ref()
        .map(|agent| {
            format!(
                " agent={} client={} runtime={} scope={}",
                agent.peer,
                agent.client_id.as_deref().unwrap_or("-"),
                agent.runtime.as_deref().unwrap_or("-"),
                agent.scope.as_deref().unwrap_or("-"),
            )
        })
        .unwrap_or_default();
    println!(
        "action={action} reason={reason:?}{card}{stale}{agent}",
        reason = recommendation.reason
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_renders_dash_when_no_claim() {
        assert_eq!(format_lease(None, 1_000), "-");
    }

    #[test]
    fn lease_renders_stale_marker_when_expired() {
        assert_eq!(format_lease(Some(500), 1_000), "<STALE>");
        // Exact-equal counts as expired — eligible for reclaim now.
        assert_eq!(format_lease(Some(1_000), 1_000), "<STALE>");
    }

    #[test]
    fn lease_renders_remaining_as_minutes_seconds() {
        // 8 minutes 12 seconds remaining.
        assert_eq!(format_lease(Some(1_000 + 8 * 60_000 + 12_000), 1_000), "8m12s");
        // Sub-minute pads seconds with leading zero.
        assert_eq!(format_lease(Some(1_000 + 5_000), 1_000), "0m05s");
    }

    #[test]
    fn short_id_truncates_to_8_chars() {
        assert_eq!(short_id("cdff6a9d-e995-4b4a-a119-10bc1faf1747"), "cdff6a9d");
        assert_eq!(short_id("short"), "short");
    }

    use airc_core::PeerId;
    use airc_work::model::CardState;
    use airc_work::{ClaimId, Priority, RepoId, WorkCard, WorkCardId};

    fn make_card(
        state: CardState,
        owner: Option<PeerId>,
        claim_id: Option<ClaimId>,
        claim_expires_at_ms: Option<u64>,
    ) -> WorkCard {
        WorkCard {
            card_id: WorkCardId::from_u128(1),
            repo: RepoId::new("test/test").unwrap(),
            title: "t".into(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            state,
            owner,
            claim_id,
            claim_expires_at_ms,
            last_heartbeat_at_ms: None,
            pull_request: None,
            created_by: PeerId::from_u128(99),
            created_at_ms: 0,
            updated_at_ms: 0,
            reviews: None,
        }
    }

    #[test]
    fn from_flags_picks_filter_or_defaults_to_all() {
        assert_eq!(BoardFilter::from_flags(false, false, false), BoardFilter::All);
        assert_eq!(BoardFilter::from_flags(true, false, false), BoardFilter::Available);
        assert_eq!(BoardFilter::from_flags(false, true, false), BoardFilter::Mine);
        assert_eq!(BoardFilter::from_flags(false, false, true), BoardFilter::Others);
    }

    #[test]
    fn available_filter_includes_open_unclaimed_and_stale_claims() {
        let me = PeerId::from_u128(10);
        let other = PeerId::from_u128(20);
        let claim = ClaimId::from_u128(30);

        // Open, no claim → available
        let card = make_card(CardState::Open, None, None, None);
        assert!(BoardFilter::Available.matches(&card, me, 1_000));

        // Open with stale claim (expired) → available for reclaim
        let card = make_card(CardState::Claimed, Some(other), Some(claim), Some(500));
        assert!(BoardFilter::Available.matches(&card, me, 1_000));

        // Open with active claim → NOT available
        let card = make_card(CardState::InProgress, Some(other), Some(claim), Some(5_000));
        assert!(!BoardFilter::Available.matches(&card, me, 1_000));

        // Closed terminal → never available (even with no claim)
        let card = make_card(CardState::Closed, None, None, None);
        assert!(!BoardFilter::Available.matches(&card, me, 1_000));
    }

    #[test]
    fn mine_and_others_split_ownership_cleanly() {
        let me = PeerId::from_u128(10);
        let other = PeerId::from_u128(20);
        let claim = ClaimId::from_u128(30);

        let mine = make_card(CardState::InProgress, Some(me), Some(claim), Some(5_000));
        let theirs = make_card(CardState::InProgress, Some(other), Some(claim), Some(5_000));
        let unclaimed = make_card(CardState::Open, None, None, None);

        assert!(BoardFilter::Mine.matches(&mine, me, 1_000));
        assert!(!BoardFilter::Mine.matches(&theirs, me, 1_000));
        assert!(!BoardFilter::Mine.matches(&unclaimed, me, 1_000));

        assert!(BoardFilter::Others.matches(&theirs, me, 1_000));
        assert!(!BoardFilter::Others.matches(&mine, me, 1_000));
        assert!(!BoardFilter::Others.matches(&unclaimed, me, 1_000));
    }

    #[test]
    fn format_peer_renders_alias_when_known_short_uuid_otherwise() {
        let me = PeerId::from_u128(10);
        let alice = PeerId::from_u128(20);
        let bob = PeerId::from_u128(30);
        let mut aliases = std::collections::HashMap::new();
        aliases.insert(alice, "alice".to_string());

        // Self is always "me", even if a stale alias exists for self.
        assert_eq!(format_peer(me, me, &aliases), "me");
        // Known peer renders by alias.
        assert_eq!(format_peer(alice, me, &aliases), "alice");
        // Unknown peer falls back to short-uuid — never empty string.
        let bob_short = format_peer(bob, me, &aliases);
        assert_eq!(bob_short.len(), 8);
        assert_ne!(bob_short, "me");
    }

    #[test]
    fn all_filter_admits_everything() {
        let me = PeerId::from_u128(10);
        let claim = ClaimId::from_u128(30);
        let cases = [
            make_card(CardState::Open, None, None, None),
            make_card(CardState::Closed, Some(me), Some(claim), Some(5_000)),
            make_card(CardState::Merged, None, None, None),
        ];
        for card in &cases {
            assert!(BoardFilter::All.matches(card, me, 1_000));
        }
    }

    // -----------------------------------------------------------------
    // Card ad7e100b Sub-B — review-card title formatting.
    //
    // Sub-C (auto-spawn on Review state) and Sub-B (this CLI) MUST
    // produce identical titles so observers filtering on
    // `title.starts_with("review:")` pick up both paths. Pin the
    // shared formatter here so a future tweak to either path can't
    // diverge them silently.
    // -----------------------------------------------------------------

    #[test]
    fn review_title_preserves_short_parent_title_verbatim() {
        let title = format_review_title("typed reviews link on WorkCard");
        assert_eq!(title, "review: typed reviews link on WorkCard");
        assert!(
            title.starts_with("review: "),
            "starts_with(\"review:\") observer filter must hold"
        );
    }

    #[test]
    fn review_title_truncates_long_parent_with_ellipsis_marker() {
        // Eighty Xs is exactly at the limit and must NOT truncate;
        // adding the 81st character must add the ellipsis.
        let parent_at_limit: String = std::iter::repeat('x').take(80).collect();
        let title_at_limit = format_review_title(&parent_at_limit);
        assert_eq!(title_at_limit, format!("review: {parent_at_limit}"));
        assert!(!title_at_limit.ends_with('…'));

        let parent_too_long: String = std::iter::repeat('x').take(120).collect();
        let title_too_long = format_review_title(&parent_too_long);
        assert!(title_too_long.ends_with('…'));
        // The visible portion + the "review: " prefix should sum to
        // the 80-char limit + the ellipsis suffix, so reviewers see
        // a recognizable parent title without the board renderer
        // wrapping.
        let expected_prefix: String = std::iter::repeat('x').take(80).collect();
        assert_eq!(title_too_long, format!("review: {expected_prefix}…"));
    }

    #[test]
    fn review_title_counts_chars_not_bytes_for_unicode_parents() {
        // Multi-byte chars are 1 char but >1 byte. The truncation
        // bound MUST be character-based; otherwise we'd slice in the
        // middle of a UTF-8 sequence and panic. Use a 3-byte glyph
        // ('日') 90 times — well past the 80-char limit but under
        // any byte-based slice.
        let parent: String = std::iter::repeat('日').take(90).collect();
        let title = format_review_title(&parent);
        // No panic = the truncation respects char boundaries. The
        // truncated portion must be exactly 80 chars of '日' + the
        // ellipsis.
        let expected_visible: String = std::iter::repeat('日').take(80).collect();
        assert_eq!(title, format!("review: {expected_visible}…"));
    }
}
