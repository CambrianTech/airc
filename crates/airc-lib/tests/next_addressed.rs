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

// what this catches (Codex, on #1692 at 4768de58): a single message addressed to Alice that arrives
// before her wake's feed is subscribed, after a stop hook saw nothing, is never heard and she idles
// deaf. The undelivered check, read after subscribing, finds it without a second send, and reading it
// delivers nothing.
#[tokio::test]
async fn a_message_sent_before_the_wake_subscribed_is_found_undelivered() {
    let machine = Machine::boot().await;
    machine.pin_identity("addressed-undelivered-test").await;
    let (alice, bob) = machine.pair_in("general").await;
    let alice_short: String = alice.peer_id().to_string().chars().take(8).collect();
    let consumer = "claude-hook:claude:alice-session";
    // The stop hook looks first and finds nothing addressed: the consumer's cursor now exists.
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None, 256)
            .await
            .expect("stop hook's look"),
        None
    );

    let addressed = bob
        .say(&format!("@{alice_short} one message, sent in the gap"))
        .await
        .expect("addressed");
    let _feed = alice.addressed_feed(None).await.expect("alice's feed");
    let woke = AddressedWake::Addressed {
        from: bob.peer_id(),
        event: addressed,
    };
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None, 256)
            .await
            .expect("check"),
        Some(woke.clone())
    );
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None, 256)
            .await
            .expect("check again"),
        Some(woke),
        "reading it delivered nothing"
    );
}
