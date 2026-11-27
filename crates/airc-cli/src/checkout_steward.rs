//! The daemon keeps its own card checkouts healthy, continuously, with no one asking.
//!
//! A card's checkout is retired when the card becomes terminal (Merged or Closed) on any node, not
//! only on the node that closed it: on the M5, 438 checkouts (185 GB) had piled up by 2026-11-27,
//! many for cards merged elsewhere, and nothing but a manual `airc work cleanup --force` would
//! ever look at them. The steward hears every card's state from every peer as typed work events
//! and retires through the same guarded path a state change on this node takes
//! (`cleanup_card_worktree`: the lifecycle lock, the board re-read under it, and retirement's own
//! proofs). At start it reconciles once, so a card that closed while the daemon was down is
//! retired too. It runs for the daemon's life and ends with it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use airc_lib::{Airc, WorkEvent, WorkEventFilter};
use airc_work::model::CardState;
use futures::StreamExt;

/// The card a work event makes terminal, if it does.
fn terminal_card(event: &WorkEvent) -> Option<airc_lib::WorkCardId> {
    match event {
        WorkEvent::CardStateChanged(changed)
            if matches!(changed.state, CardState::Merged | CardState::Closed) =>
        {
            Some(changed.card_id)
        }
        WorkEvent::PullRequestMerged(merged) => Some(merged.card_id),
        _ => None,
    }
}

/// Run until `shutdown`: once the daemon serves (`bound`), attach as one of its clients, reconcile
/// every existing checkout, then retire each card's checkout as its terminal event arrives.
pub(crate) async fn run(
    home: PathBuf,
    socket: PathBuf,
    bound: Arc<tokio::sync::Notify>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    // A temp-rooted scope (a test's daemon) shares the machine's checkout root but owns none of it.
    if airc_core::scope_home_is_temp_rooted(&home) {
        return;
    }
    bound.notified().await;
    let airc = match Airc::attach(&home, socket).await {
        Ok(airc) => airc,
        Err(error) => {
            eprintln!("airc daemon: checkout steward could not attach to its own daemon: {error}");
            return;
        }
    };
    let mut events = match airc.subscribe_work_events(WorkEventFilter::default()).await {
        Ok(events) => Box::pin(events),
        Err(error) => {
            eprintln!("airc daemon: checkout steward could not subscribe to work events: {error}");
            return;
        }
    };
    // Subscribed first, so a card that turns terminal during the reconciliation is still heard.
    reconcile(&home).await;
    loop {
        tokio::select! {
            () = shutdown.notified() => return,
            next = events.next() => match next {
                Some((_, event)) => {
                    if let Some(card) = terminal_card(&event) {
                        retire(&airc, card).await;
                    }
                }
                None => {
                    eprintln!("airc daemon: checkout steward's work feed closed");
                    return;
                }
            },
        }
    }
}

async fn reconcile(home: &Path) {
    if let Err(error) = crate::work_commands::run_cleanup(home, false, true).await {
        eprintln!("airc daemon: checkout steward's start reconciliation failed: {error}");
    }
}

async fn retire(airc: &Airc, card: airc_lib::WorkCardId) {
    if let Err(error) = crate::work_commands::cleanup_card_worktree(airc, card, None).await {
        eprintln!("airc daemon: checkout steward retained {card}'s checkout: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a checkout retired while its card can still be worked (claimed, in
    // review, reopened), or one never retired after its card merged or closed on another node.
    #[test]
    fn only_a_merged_or_closed_card_is_retired() {
        let card = airc_lib::WorkCardId::from_uuid(uuid::Uuid::from_u128(7));
        let peer = airc_core::PeerId::from_uuid(uuid::Uuid::from_u128(9));
        let changed = |state| {
            WorkEvent::CardStateChanged(airc_work::event::CardStateChanged {
                card_id: card,
                state,
                changed_by: peer,
                changed_at_ms: 1,
            })
        };
        assert_eq!(terminal_card(&changed(CardState::Merged)), Some(card));
        assert_eq!(terminal_card(&changed(CardState::Closed)), Some(card));
        assert_eq!(terminal_card(&changed(CardState::Review)), None);
        assert_eq!(terminal_card(&changed(CardState::Claimed)), None);
        assert_eq!(terminal_card(&changed(CardState::Open)), None);
    }
}
