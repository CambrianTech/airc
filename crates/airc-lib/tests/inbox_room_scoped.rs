//! Regression for continuum #270: reading a subscribed room must not
//! require MOVING this scope's default-room pointer.
//!
//! `airc msg`, `airc send` and `airc publish` each took `--room <name>`
//! for one-shot routing. `airc inbox` — the READ — took no such flag and
//! could only ever show whichever room the scope happened to be sitting
//! in, so the documented way to read one of your other subscribed rooms
//! was `airc room <name>`: a WRITE to shared scope state performed in
//! order to do a read. Two rooms, two agents, one pointer — whoever read
//! last silently relocated everyone.
//!
//! What this catches: a `page_recent`/`resume_from` that reaches for
//! `current_room()` internally instead of honoring the room it was
//! handed, and any future re-plumbing of `inbox --room` back through the
//! default-room pointer. The load-bearing assertion is not that the
//! events come back — it is that `peek_default_room` is UNCHANGED
//! afterwards, and still unchanged after a reopen (a mutation that only
//! lived in memory would still be a mutation).

use airc_core::Body;
use airc_lib::{Airc, PublishTarget};
use airc_protocol::FrameKind;
use tempfile::tempdir;

#[tokio::test]
async fn reading_another_subscribed_room_does_not_move_the_default_room_pointer() {
    let dir = tempdir().unwrap();
    let machine = dir.path().join("machine/.airc");
    let wire = dir.path().join("wire");

    let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("open scope");

    // Two subscribed rooms. `current_room` lands the scope in the first
    // one and pins it as default; the second is joined but not current —
    // exactly the shape that made inbox blind.
    let here = airc.current_room().await.expect("current room");
    airc.join("elsewhere").await.expect("join second room");
    // `join` MOVES the scope into the room it joins, so come back. Without
    // this the scope is already sitting in 'elsewhere' and the whole test
    // is vacuous — a `page_recent_in` that ignored its argument and read
    // `current_room` would pass. (It did: caught by positive control.)
    airc.join(&here.name).await.expect("return to the first room");
    assert_eq!(
        airc.current_room().await.expect("current room").channel,
        here.channel,
        "setup: the scope must be sitting in the FIRST room before the read"
    );

    // Put a message in the room we are NOT sitting in.
    airc.publish(
        PublishTarget::RoomByName("elsewhere".to_string()),
        FrameKind::Message,
        Body::text("message that lives in the other room"),
        std::collections::BTreeMap::new(),
    )
    .await
    .expect("publish to the non-current room");

    let default_before = airc
        .peek_default_room()
        .await
        .expect("default room before the read");

    // THE READ under test: page the other room explicitly.
    let there = airc
        .room_by_name_or_channel("elsewhere", "read")
        .await
        .expect("resolve the other room");
    let events = airc.page_recent_in(&there, 32).await.expect("page the other room");

    assert!(
        events.iter().any(|event| {
            event
                .body
                .as_ref()
                .and_then(airc_core::Body::as_text)
                .is_some_and(|text| text.contains("message that lives in the other room"))
        }),
        "a room-scoped read must return that room's events — got {} event(s) instead",
        events.len()
    );
    assert_ne!(
        there.channel, here.channel,
        "test is vacuous unless the room read differs from the current one"
    );

    // The invariant. Reading is not moving.
    let default_after = airc
        .peek_default_room()
        .await
        .expect("default room after the read");
    assert_eq!(
        default_before, default_after,
        "reading room 'elsewhere' moved this scope's default-room pointer — a READ must \
         never mutate shared scope state (#270)"
    );

    // And it did not merely fail to mutate in memory: a fresh handle on
    // the same home agrees.
    drop(airc);
    let reopened = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("reopen same home");
    assert_eq!(
        reopened
            .peek_default_room()
            .await
            .expect("default room after reopen"),
        default_after,
        "the default-room pointer changed on disk across a room-scoped read"
    );
}
