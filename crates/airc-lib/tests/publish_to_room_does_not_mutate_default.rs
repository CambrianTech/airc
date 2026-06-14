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
