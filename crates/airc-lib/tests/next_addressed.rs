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
            .addressed_undelivered(consumer, None)
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
            .addressed_undelivered(consumer, None)
            .await
            .expect("check"),
        Some(woke.clone())
    );
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None)
            .await
            .expect("check again"),
        Some(woke),
        "reading it delivered nothing"
    );
}

// what this catches (Codex on #1692 at 7d371beb): the undelivered check read one unfiltered page, so
// an addressed message behind a page of others was missed and the wake waited forever with it
// already stored. After the stop hook's empty look, sixty unaddressed lines and then one addressed
// line arrive before the wake subscribes, and nothing is sent after: the check still finds it.
#[tokio::test]
async fn an_addressed_message_behind_a_page_of_others_is_found_undelivered() {
    let machine = Machine::boot().await;
    machine.pin_identity("addressed-behind-a-page-test").await;
    let (alice, bob) = machine.pair_in("general").await;
    let alice_short: String = alice.peer_id().to_string().chars().take(8).collect();
    let consumer = "claude-hook:claude:alice-busy-room";
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None)
            .await
            .expect("stop hook's look"),
        None
    );

    for i in 0..60 {
        bob.say(&format!("ambient line {i}"))
            .await
            .expect("ambient");
    }
    let addressed = bob
        .say(&format!("@{alice_short} behind sixty others"))
        .await
        .expect("addressed");
    let _feed = alice.addressed_feed(None).await.expect("alice's feed");
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None)
            .await
            .expect("check"),
        Some(AddressedWake::Addressed {
            from: bob.peer_id(),
            event: addressed,
        })
    );
}

// what this catches: a wake that fires again for a message the session was already shown, so every
// re-armed wake exits at once and the session loops. Once a hook delivers it, it is no longer owed.
#[tokio::test]
async fn a_delivered_addressed_message_is_no_longer_owed() {
    let machine = Machine::boot().await;
    machine.pin_identity("addressed-delivered-test").await;
    let (alice, bob) = machine.pair_in("general").await;
    let alice_short: String = alice.peer_id().to_string().chars().take(8).collect();
    let consumer = "claude-hook:claude:alice-delivered";
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None)
            .await
            .expect("first look"),
        None
    );

    bob.say(&format!("@{alice_short} read me once"))
        .await
        .expect("addressed");
    assert!(alice
        .addressed_undelivered(consumer, None)
        .await
        .expect("owed")
        .is_some());

    let turn = alice
        .prepare_agent_delivery(consumer, 50, 10)
        .await
        .expect("a hook's turn");
    let shown = turn.live_events().len();
    turn.acknowledge(shown, false).await.expect("delivered");
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None)
            .await
            .expect("after delivery"),
        None
    );
}

// what this catches (the M5, 2026-11-27): a wake armed while a hook turn held the consumer's
// delivery lock exited "consumer busy" at once, and the session idled deaf. The wake's backlog look
// waits for the turn to end, then finds what was owed: a hook turn holds the lock, Bob addresses
// Alice, the look stays pending until the turn drops, and then names his message.
#[tokio::test]
async fn the_wakes_look_waits_for_a_hook_turn_then_finds_what_is_owed() {
    let machine = Machine::boot().await;
    machine.pin_identity("addressed-wait-test").await;
    let (alice, bob) = machine.pair_in("general").await;
    let alice_short: String = alice.peer_id().to_string().chars().take(8).collect();
    let consumer = "claude-hook:claude:alice-session";
    assert_eq!(
        alice
            .addressed_undelivered(consumer, None)
            .await
            .expect("the first look"),
        None
    );
    let turn = alice
        .prepare_agent_delivery(consumer, 1, 1)
        .await
        .expect("a hook turn holds the lock");
    let addressed = bob
        .say(&format!("@{alice_short} sent while a hook delivered"))
        .await
        .expect("addressed");
    let looking = {
        let alice = alice.clone();
        tokio::spawn(async move { alice.addressed_undelivered(consumer, None).await })
    };
    // the turn still holds the lock: the look waits, it neither fails nor answers. The pause only
    // gives the look time to reach the lock (a refusing look ends within it); a slow runner can make
    // this pass without proving the wait, never fail it falsely.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !looking.is_finished(),
        "the look ended while a hook turn held the lock"
    );
    turn.acknowledge(0, false)
        .await
        .expect("the hook turn ends");
    assert_eq!(
        looking.await.expect("joined").expect("the look"),
        Some(AddressedWake::Addressed {
            from: bob.peer_id(),
            event: addressed,
        })
    );
}
