//! Where each work id lives, so a shown id resolves with [`Shown::resolve_in`]
//! against the data that already names it: a room's board holds its cards and
//! their claims, a card holds its claim and its submissions.
//!
//! [`Shown::resolve_in`]: airc_core::shown_id::Shown::resolve_in

use airc_core::shown_id::ShownScope;

use crate::ids::{ClaimId, SubmissionId, WorkCardId};
use crate::model::WorkCard;
use crate::projection::BoardSnapshot;

impl ShownScope<WorkCardId> for BoardSnapshot {
    fn shown_ids(&self) -> Vec<WorkCardId> {
        self.cards.iter().map(|card| card.card_id).collect()
    }
}

impl ShownScope<ClaimId> for BoardSnapshot {
    fn shown_ids(&self) -> Vec<ClaimId> {
        self.cards.iter().filter_map(|card| card.claim_id).collect()
    }
}

/// A card's own claim: the narrowest population a claim handle can name.
impl ShownScope<ClaimId> for WorkCard {
    fn shown_ids(&self) -> Vec<ClaimId> {
        self.claim_id.into_iter().collect()
    }
}

impl ShownScope<SubmissionId> for WorkCard {
    fn shown_ids(&self) -> Vec<SubmissionId> {
        self.submissions
            .iter()
            .map(|submission| submission.submission_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use airc_core::shown_id::Shown;

    use crate::event::{CardCreated, ClaimOrigin, WorkCardClaimed, WorkEvent};
    use crate::ids::{ClaimId, RepoId, WorkCardId};
    use crate::model::Priority;
    use crate::projection::WorkBoardProjection;
    use airc_core::PeerId;

    // what this catches: a board resolves the card and claim handles it shows, from its own
    // cards, and a card resolves only its own claim, so a verb needs no candidate list.
    #[test]
    fn a_board_and_a_card_resolve_the_ids_they_show() {
        let (held, open) = (
            WorkCardId::from_u128(0xabcdef01 << 96),
            WorkCardId::from_u128(0x12345678 << 96),
        );
        let claim = ClaimId::from_u128(0x5eed0001 << 96);
        let owner = PeerId::from_u128(3);
        let mut projection = WorkBoardProjection::new();
        for card_id in [held, open] {
            projection
                .apply(&WorkEvent::CardCreated(CardCreated {
                    card_id,
                    repo: RepoId::new("CambrianTech/airc").unwrap(),
                    title: "scope".to_string(),
                    body: None,
                    priority: Priority::P2,
                    lane_id: None,
                    created_by: owner,
                    created_at_ms: 100,
                    reviews: None,
                    origin: None,
                }))
                .unwrap();
        }
        projection
            .apply(&WorkEvent::CardClaimed(WorkCardClaimed {
                selected_at_ms: None,
                card_id: held,
                claim_id: claim,
                owner,
                ttl_ms: 100,
                claimed_at_ms: 110,
                origin: ClaimOrigin::Explicit,
            }))
            .unwrap();
        let board = projection.snapshot();

        let card = |raw: &str| Shown::<WorkCardId>::try_from(raw).unwrap();
        let claim_shown = Shown::<ClaimId>::try_from(&*claim.shown()).unwrap();
        assert_eq!(card(&held.shown()).resolve_in(&board), Ok(held));
        assert_eq!(card(&open.shown()).resolve_in(&board), Ok(open));
        assert_eq!(
            claim_shown.resolve_in(&board),
            Ok(claim),
            "the board names its claims"
        );
        let held_card = board.cards.iter().find(|c| c.card_id == held).unwrap();
        let open_card = board.cards.iter().find(|c| c.card_id == open).unwrap();
        assert_eq!(
            claim_shown.resolve_in(held_card),
            Ok(claim),
            "a card names its own claim"
        );
        assert!(
            claim_shown.resolve_in(open_card).is_err(),
            "and no other card's"
        );
    }
}
