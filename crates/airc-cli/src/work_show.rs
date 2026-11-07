//! One work card as a reader sees it: the board's row for every card, and `airc work show` for one
//! card in full, body included. A card's body is where its repro, measurements and acceptance live;
//! the board omits it, and without `show` a claimant could not read the work it claimed (card
//! cc99488d).

use std::collections::HashMap;
use std::path::Path;

use airc_lib::{ClaimId, PeerId};

use crate::work_commands::{card_in_room, format_lease, format_peer, now_ms, room_or_current};

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
    let card_uuid = card_in_room(&airc, &room, &card_id).await?;
    let board = airc.work_board_in(&room).await?;
    let card = board
        .card(card_uuid)
        .ok_or_else(|| format!("card {card_uuid} not visible in room {}", room.name))?;
    let me = airc.peer_id();
    let mut aliases = HashMap::new();
    let peers = card.owner.into_iter().chain(
        card.submissions
            .iter()
            .map(|submission| submission.publisher),
    );
    for peer in peers.filter(|peer| *peer != me) {
        if let Ok(Some(alias)) = airc.peer_alias(peer).await {
            aliases.insert(peer, alias);
        }
    }
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
