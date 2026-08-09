//! A wall must be readable for a NAMED room, and the answer must be that
//! room's posts — never the current room's.
//!
//! Same correctness hazard as the room-scoped work board (Continuum #345), on
//! the other projection of the same transcript: `wall_posts` read through
//! `page_recent`, which is current-room-only, so a caller that meant a specific
//! room got a *plausible* wall for the wrong one rather than an error.
//!
//! This matters because Continuum reads a room's STANDING off its wall — "is
//! this activity concluded?" A persona reacts to traffic from any room it
//! subscribes to, not only its default one, so the current-room read answers
//! about the wrong room in exactly the case where the two differ, which is
//! exactly when the question is being asked.
//!
//! What this catches: a regression where the scoped read falls back to
//! `current_room()`, or where the two rooms' posts bleed together. Each room
//! below holds a post the other does not, so a fallback cannot pass by
//! coincidence.

mod common;

use airc_lib::Airc;
use common::Machine;

const CATEGORY: &str = "standing";

async fn publish(airc: &Airc, body: &str) {
    airc.publish_wall_post(CATEGORY.to_string(), body.to_string(), None)
        .await
        .expect("publish wall post");
}

async fn bodies(airc: &Airc, room: &airc_lib::Room) -> Vec<String> {
    let mut bodies: Vec<String> = airc
        .wall_posts_in(room, Some(CATEGORY))
        .await
        .expect("scoped wall read")
        .into_iter()
        .map(|post| post.body)
        .collect();
    bodies.sort();
    bodies
}

#[tokio::test]
async fn a_wall_read_for_a_named_room_returns_that_rooms_posts_not_the_current_rooms() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;

    // Two rooms, one distinct post each. Posts land in whatever room is
    // current at publish time, so publish-then-switch.
    let other = alice
        .join("other-room")
        .await
        .expect("alice joins other-room");
    publish(&alice, "OTHER-ROOM STANDING").await;
    let home = alice
        .join("home-room")
        .await
        .expect("alice joins home-room");
    publish(&alice, "HOME-ROOM STANDING").await;

    // Precondition: the default read answers for home-room. If this ever
    // fails the rest of the test proves nothing, so assert it explicitly.
    let default: Vec<String> = alice
        .wall_posts(Some(CATEGORY))
        .await
        .expect("default wall")
        .into_iter()
        .map(|post| post.body)
        .collect();
    assert_eq!(
        default,
        vec!["HOME-ROOM STANDING".to_string()],
        "test precondition: the unscoped wall reads the current (home) room"
    );

    // The actual contract: ask for the OTHER room while pointed at home, and
    // get the other room's wall. A current-room fallback returns the home post
    // here — a plausible answer, and the wrong one.
    assert_eq!(
        bodies(&alice, &other).await,
        vec!["OTHER-ROOM STANDING".to_string()],
        "a wall read for a named room must answer for THAT room"
    );

    // And naming the current room explicitly still works — the scoped path is
    // the only implementation, so this pins that `wall_posts` delegating to it
    // did not change what the default read means.
    assert_eq!(
        bodies(&alice, &home).await,
        vec!["HOME-ROOM STANDING".to_string()],
        "naming the current room reads the same wall the default read does"
    );
}

/// what this catches: the category filter silently widening when the read went
/// room-scoped. A standing read must not pick up a room's other pinned posts —
/// if it did, "is this room concluded?" would parse an unrelated body and, per
/// `current_standing`'s hard-error-on-unparseable rule, fail the whole turn.
#[tokio::test]
async fn a_scoped_wall_read_still_honors_the_category_filter() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;

    let room = alice.join("filtered-room").await.expect("alice joins");
    publish(&alice, "STANDING POST").await;
    alice
        .publish_wall_post("doctrine".to_string(), "DOCTRINE POST".to_string(), None)
        .await
        .expect("publish doctrine post");

    assert_eq!(
        bodies(&alice, &room).await,
        vec!["STANDING POST".to_string()],
        "the standing category must not pick up a doctrine post"
    );

    let all: Vec<String> = alice
        .wall_posts_in(&room, None)
        .await
        .expect("unfiltered scoped wall")
        .into_iter()
        .map(|post| post.body)
        .collect();
    assert_eq!(
        all.len(),
        2,
        "unfiltered, the same scoped read sees both posts: {all:?}"
    );
}
