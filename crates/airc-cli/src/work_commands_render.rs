//! Board / roster / manager terminal renderers.
//!
//! Card 47849771 (phase 1 redo, originally c0bd865c). Pure terminal
//! formatting — no substrate calls, no state mutation — so the
//! split is structurally clean. Subsequent extractions (phases 2/3/
//! 4/5 — lifecycle/git/gh/review) already shipped.

use airc_lib::{
    WorkBoardProjection, WorkManagerRecommendation, WorkManagerRecommendationKind,
    WorkManagerStatus, WorkQueueStatus, WorkRosterStatus,
};

use crate::work_commands::{format_lease, format_peer, now_ms, short_id, BoardFilter};

pub(crate) fn print_board(
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

pub(crate) fn print_work_queue_availability(status: &WorkQueueStatus) {
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

pub(crate) fn print_work_roster(status: &WorkRosterStatus) {
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

pub(crate) fn print_work_manager(status: &WorkManagerStatus) {
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

pub(crate) fn print_manager_recommendation(recommendation: &WorkManagerRecommendation) {
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
