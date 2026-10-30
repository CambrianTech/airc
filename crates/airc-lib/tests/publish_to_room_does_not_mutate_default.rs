//! Regression test for card a979e5c2 (seam #5): the `--room <name>` one-shot
//! send must NEVER mutate this scope's default-room pointer.
//!
//! The CLI surface (`airc msg --room X "..."` and `airc send --room X "..."`)
//! routes through `airc.publish(PublishTarget::RoomByName(name), ...)`. The
//! invariant — "after a one-shot --room send, `airc room` still reports the
//! ORIGINAL room" — is what makes cross-channel coordination work without
//! silently flipping the user's current-room pointer.
//!
//! BIGMAMA's adversarial review of PR #1188 verified the impl by READING the
//! code (no mutation in the publish path) — but a future refactor could
//! silently reintroduce the room-drift bug with a green suite. This test pins
//! the invariant structurally so any drift fails CI.

mod common;

use airc_core::Body;
use airc_lib::PublishTarget;
use airc_protocol::FrameKind;
use common::Machine;

#[tokio::test]
async fn publish_to_room_by_name_does_not_change_current_room() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;

    // Alice joins TWO rooms. The first one becomes her current/default
    // because join semantics in this fixture leave the most-recent join
    // wired as current. Re-join the "home" room AFTER to anchor it as
    // current — that's the room the no-mutation invariant must protect.
    let other = alice
        .join("other-room")
        .await
        .expect("alice joins other-room");
    let home = alice
        .join("home-room")
        .await
        .expect("alice joins home-room");
    let current_before = alice
        .current_room()
        .await
        .expect("read current room before");
    assert_eq!(
        current_before.name, home.name,
        "test precondition: current room should be the most-recently-joined home-room"
    );

    // The act under test: one-shot publish to the OTHER room. This is the
    // exact code path `airc msg --room other-room` exercises.
    let receipt = alice
        .publish(
            PublishTarget::RoomByName(other.name.clone()),
            FrameKind::Message,
            airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            Body::text("targeted at other-room, sender stays on home-room"),
            airc_core::Headers::new(),
        )
        .await
        .expect("publish to other-room");
    assert_eq!(
        receipt.channel_name, other.name,
        "publish routed to the requested room (sanity)"
    );

    // The invariant: current room is UNCHANGED.
    let current_after = alice.current_room().await.expect("read current room after");
    assert_eq!(
        current_after.name, current_before.name,
        "BUG: --room publish silently mutated the default-room pointer (regression of seam #5)"
    );
    assert_eq!(
        current_after.channel, current_before.channel,
        "BUG: --room publish silently changed the channel id (regression of seam #5)"
    );
}

// Real daemon IPC must preserve the recipient, not silently broadcast it.
#[tokio::test]
async fn directed_publish_preserves_peer_and_room_through_daemon() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;
    let bob = machine.attach("bob").await;
    let other = alice.join("directed-other").await.expect("join other");
    bob.join("directed-other").await.expect("recipient joins");
    let home = alice.join("directed-home").await.expect("join home");
    let receipt = alice
        .publish(
            PublishTarget::RoomByName(other.name.clone()).to_peer(bob.peer_id()),
            FrameKind::Message,
            airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            Body::text("cross-room directed challenge"),
            airc_core::Headers::new(),
        )
        .await
        .expect("directed publish");
    let events = bob.page_recent_in(&other, 20).await.expect("read receipt");
    let event = events
        .iter()
        .find(|event| event.event_id == receipt.event_id)
        .expect("persisted event");
    assert_eq!(event.target, airc_core::MentionTarget::Peer(bob.peer_id()));
    assert_eq!(event.room_id, other.channel);
    assert_eq!(
        alice.current_room().await.expect("current").channel,
        home.channel
    );
    assert!(alice
        .publish(
            PublishTarget::RoomByName("not-subscribed".into()).to_peer(bob.peer_id()),
            FrameKind::Message,
            airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            Body::text("must refuse"),
            airc_core::Headers::new(),
        )
        .await
        .is_err());
}

#[tokio::test]
async fn directed_publish_preserves_recipient_without_daemon() {
    let dir = tempfile::tempdir().expect("home");
    let alice = airc_lib::Airc::open(dir.path()).await.expect("open");
    let room = alice.join("direct-self").await.expect("join");
    let receipt = alice
        .publish(
            PublishTarget::CurrentRoom.to_peer(alice.peer_id()),
            FrameKind::Message,
            airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            Body::text("typed self message"),
            airc_core::Headers::new(),
        )
        .await
        .expect("publish");
    let events = alice.page_recent_in(&room, 20).await.expect("history");
    let event = events
        .iter()
        .find(|event| event.event_id == receipt.event_id)
        .expect("persisted event");
    assert_eq!(
        event.target,
        airc_core::MentionTarget::Peer(alice.peer_id())
    );
}
