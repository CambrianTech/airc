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

use airc_lib::{ClaimWorkCard, CreateWorkCard, Priority, RepoId, WorkEventFilter};
use airc_work::WorkEvent;
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

    // The release is the record of the takeover: by bob, of alice's claim, with alice
    // TYPED as the holder it was taken from, never spelled into the reason.
    let release = bob
        .recent_work_events(WorkEventFilter::new(), 200)
        .await
        .expect("bob reads the room's work events")
        .into_iter()
        .find_map(|event| match event {
            WorkEvent::ClaimReleased(r) if r.card_id == card_id => Some(r),
            _ => None,
        })
        .expect("the takeover published a release");
    assert_eq!(release.claim_id, alices, "it released alice's claim");
    assert_eq!(release.owner, bob.peer_id(), "released by the taker");
    assert_eq!(
        release.taken_over_from,
        Some(alice.peer_id()),
        "typed holder"
    );
    let reason = release.reason.expect("a takeover says why");
    assert!(
        !reason.contains(&alice.peer_id().to_string())
            && !reason.contains(&bob.peer_id().to_string()),
        "no id inside the reason: {reason}"
    );
}
