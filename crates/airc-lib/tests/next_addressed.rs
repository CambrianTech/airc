//! An idle agent hears what is addressed to it (docs/AGENT-DELIVERY.md): the wake's core.

mod common;

use airc_lib::AddressedWake;
use common::Machine;

// what this catches: a wake that never fires for a message addressed to its reader (the agent idles
// deaf), or one that fires for traffic addressed to someone else (it wakes for nothing). Alice's feed
// is subscribed before Bob speaks; his ambient line and his line to another name come first, so the
// wake must end on his line addressed to Alice, by its event id.
#[tokio::test]
async fn a_wake_ends_on_the_first_message_addressed_to_its_reader_and_no_other() {
    let machine = Machine::boot().await;
    machine.pin_identity("next-addressed-test").await;
    let (alice, bob) = machine.pair_in("general").await;
    let mut feed = alice.addressed_feed(None).await.expect("alice's feed");
    let alice_short: String = alice.peer_id().to_string().chars().take(8).collect();

    bob.say("an ambient line for the room")
        .await
        .expect("ambient");
    bob.say("@nobody-here a line for someone else")
        .await
        .expect("other");
    let addressed = bob
        .say(&format!("@{alice_short} are you there?"))
        .await
        .expect("addressed");

    assert_eq!(
        feed.next().await,
        AddressedWake::Addressed {
            from: bob.peer_id(),
            event: addressed,
        }
    );
}
