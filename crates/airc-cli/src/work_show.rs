//! One work card as a reader sees it: the board's row for every card, and `airc work show` for one
//! card in full, body included. A card's body is where its repro, measurements and acceptance live;
//! the board omits it, and without `show` a claimant could not read the work it claimed (card
//! cc99488d).

use std::collections::HashMap;
use std::path::Path;

use airc_lib::{ClaimId, PeerId};

use crate::work_commands::{card_on_board, now_ms, room_or_current};

/// Render a peer-id for the board: 'me' for self, the published alias
/// when known (kink 6f111211 / card c397567a — looked up via
/// Airc::peer_alias and pre-fetched into the map by run_board), else
/// short-uuid fallback. Honest "unknown" rendering keeps the board
/// readable even when a peer hasn't published an identity card to
/// this room yet.
pub(crate) fn format_peer(peer: PeerId, me: PeerId, aliases: &HashMap<PeerId, String>) -> String {
    if peer == me {
        "me".to_string()
    } else if let Some(alias) = aliases.get(&peer) {
        alias.clone()
    } else {
        peer.shown()
    }
}

/// Render claim-lease liveness inline on the board (kink ac6affc7):
/// '-' = no claim, '<STALE>' = lease expired (eligible for reclaim per
/// the flywheel-continuity doctrine), otherwise time remaining as
/// 'Mm SS s'. Lets at-a-glance scanning of the board surface stale
/// claims without having to cross-reference 'work roster' + ttl.
pub(crate) fn format_lease(expires_at_ms: Option<u64>, now_ms: u64) -> String {
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

/// The published alias of every peer in `peers` other than `me` (the durable identity index, so
/// presence does not matter). A peer whose alias cannot be read is left out, and [`format_peer`]
/// renders it by its short id: a missing alias costs a readable name, never the row.
pub(crate) async fn aliases_for(
    airc: &airc_lib::Airc,
    peers: impl Iterator<Item = PeerId>,
) -> HashMap<PeerId, String> {
    let me = airc.peer_id();
    let mut aliases = HashMap::new();
    for peer in peers.filter(|peer| *peer != me) {
        if let Ok(Some(alias)) = airc.peer_alias(peer).await {
            aliases.insert(peer, alias);
        }
    }
    aliases
}

/// One card as the board lists it: its row, then its latest submission and rejection when present.
/// Shared by `work board` (every card) and `work show` (one card, then its body).
pub(crate) fn print_card_row(
    card: &airc_work::WorkCard,
    me: PeerId,
    aliases: &HashMap<PeerId, String>,
    now: u64,
) {
    let owner = card
        .owner
        .map(|peer| format_peer(peer, me, aliases))
        .unwrap_or_else(|| "-".to_string());
    let claim = card
        .claim_id
        .map(ClaimId::shown)
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
    if let Some(submission) = card.latest_submission() {
        println!(
            "  submissions={} latest={} publisher={} base={} sha256={} bytes={} (artifact reference; availability not asserted)",
            card.submissions.len(),
            submission.submission_id,
            format_peer(submission.publisher, me, aliases),
            submission.base_sha,
            submission.artifact.hash,
            submission.artifact.size_bytes,
        );
    }
    if let Some(rejected) = &card.last_submission_rejection {
        println!(
            "  submission_rejected={} reason={}",
            rejected.submission_id, rejected.reason
        );
    }
}

/// `airc work show <CARD>`: one card in full, its board row, room, pull request, review link and
/// body. Resolved in `--room` like every other card verb.
pub async fn run_show(
    home: &Path,
    room: Option<String>,
    card_id: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let room = room_or_current(&airc, room.as_deref(), "show work in").await?;
    let board = airc.work_board_in(&room).await?;
    let card_uuid = card_on_board(
        &board,
        &airc_core::shown_id::Shown::try_from(card_id.as_str())?,
    )?;
    let card = board
        .card(card_uuid)
        .ok_or_else(|| format!("card {card_uuid} not visible in room {}", room.name))?;
    let me = airc.peer_id();
    let peers = card.owner.into_iter().chain(
        card.submissions
            .iter()
            .map(|submission| submission.publisher),
    );
    let aliases = aliases_for(&airc, peers).await;
    print_card_row(card, me, &aliases, now_ms());
    println!("room: #{} [{}]", room.name, room.channel);
    if let Some(pr) = &card.pull_request {
        println!(
            "pull_request: {}#{} head={} base={}",
            pr.repo, pr.number, pr.head, pr.base
        );
    }
    if let Some(parent) = card.reviews {
        println!("reviews: {parent}");
    }
    println!();
    println!("{}", card.body.as_deref().unwrap_or("(no body)"));
    Ok(())
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
        assert_eq!(
            format_lease(Some(1_000 + 8 * 60_000 + 12_000), 1_000),
            "8m12s"
        );
        // Sub-minute pads seconds with leading zero.
        assert_eq!(format_lease(Some(1_000 + 5_000), 1_000), "0m05s");
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
}
