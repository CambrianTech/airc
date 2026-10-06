//! Joel, 2026-10-06: "allow anyone to take it at any time from him ... Last thing we
//! want is stopped flywheel." A persona that went dormant (Solomon, on the M5) held
//! bench cards nobody could take, because ownership was durable (card d826e5f1): a
//! stranger's claim was refused at the gate and dropped by the projection.
//!
//! What this catches: a held card, LIVE lease included, that a second peer cannot take,
//! or a takeover that leaves the holder's claim standing. The taker's claim must land
//! on the board through the projection's post-cutover rule (which still drops a bare
//! stranger's claim), so the attributed release has to come first.

mod common;

use airc_lib::{ClaimWorkCard, CreateWorkCard, Priority, RepoId};
use common::Machine;

#[tokio::test]
async fn a_second_peer_takes_a_live_held_card_and_the_holders_claim_is_released() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;
    let bob = machine.attach("bob").await;
    let room = alice.join("bench-room").await.expect("alice joins");
    bob.join("bench-room").await.expect("bob joins");

    let card_id = alice
        .create_work_card(CreateWorkCard::new(
            RepoId::new("test-org/test-repo").unwrap(),
            "held card",
            Priority::P1,
        ))
        .await
        .expect("create");
    let alices = alice
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 3_600_000, // live for the whole test
        })
        .await
        .expect("alice claims");

    let bobs = bob
        .claim_work_card(ClaimWorkCard {
            card_id,
            ttl_ms: 60_000,
        })
        .await
        .expect("bob takes over a live claim");
    assert_ne!(bobs, alices);

    for (who, airc) in [("alice", &alice), ("bob", &bob)] {
        let card = airc
            .work_board_in(&room)
            .await
            .expect("board")
            .snapshot()
            .cards
            .into_iter()
            .find(|c| c.card_id == card_id)
            .expect("the card");
        assert_eq!(
            card.owner,
            Some(bob.peer_id()),
            "{who}'s board: bob holds it now"
        );
        assert_eq!(
            card.claim_id,
            Some(bobs),
            "{who}'s board: bob's claim, not alice's"
        );
    }
}
