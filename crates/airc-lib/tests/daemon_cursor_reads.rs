//! Card 8428ae8c — daemon-attached cursor reads ask the DAEMON.
//!
//! `Airc::latest_cursor` / `Airc::subscription_cursor` used to read the
//! LOCAL store even when the scope was daemon-attached. Under the
//! owner-core model the daemon's ORM is the transcript and an attached
//! scope's local store can be empty or stale — so those reads answered
//! `None` (or an old cursor) for rooms with live daemon history. With
//! the typed O(1) `room_tip` op (card a1562dbc) asking the daemon is as
//! cheap as the local index, so attached instances now route through
//! it. Local (non-attached) instances are unchanged.
//!
//! The discriminating actor is a scope that never wrote anything to its
//! own store: if the routing regresses to the local read, every
//! assertion below sees `None` instead of the daemon's tip.

mod common;

use airc_lib::ChannelName;
use common::Machine;

#[tokio::test]
async fn attached_latest_cursor_is_the_daemon_tip_not_the_local_store() {
    let machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("cursor-reads").await;

    // History exists only in the DAEMON's ORM — neither scope's local
    // store has these events.
    alice.say("one").await.expect("say one");
    alice.say("two").await.expect("say two");
    let last = alice.say("three").await.expect("say three");

    // Bob never wrote anything; his local store knows nothing. The
    // daemon-attached read must still see the room tip.
    let cursor = bob
        .latest_cursor()
        .await
        .expect("latest_cursor")
        .expect("attached read sees the daemon's history");
    assert_eq!(
        cursor.event_id, last,
        "latest_cursor is the daemon's newest durable, not local state"
    );

    // And it agrees with the sender's view of the same room.
    let alice_cursor = alice
        .latest_cursor()
        .await
        .expect("latest_cursor")
        .expect("sender sees the tip too");
    assert_eq!(alice_cursor, cursor, "both scopes read the same tip");
}

#[tokio::test]
async fn attached_subscription_cursor_is_the_daemon_tip() {
    let machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("sub-cursor").await;

    let last = alice.say("newest").await.expect("say");

    let channel = ChannelName::new("sub-cursor").expect("channel name");
    let cursor = bob
        .subscription_cursor(&channel)
        .await
        .expect("subscription_cursor")
        .expect("subscribed channel with daemon history has a cursor");
    assert_eq!(
        cursor.event_id, last,
        "subscription_cursor is the daemon's room tip"
    );

    // A channel this scope is not subscribed to still answers None —
    // the subscription gate is unchanged by the daemon routing.
    let unsubscribed = ChannelName::new("not-joined").expect("channel name");
    assert_eq!(
        bob.subscription_cursor(&unsubscribed)
            .await
            .expect("subscription_cursor"),
        None,
        "unsubscribed channel stays None"
    );
}
