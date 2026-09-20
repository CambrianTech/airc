//! The wall is a cached projection, resumed strictly after its snapshot cursor —
//! the second instance of the work board's cache (2026-09-06: a from-zero wall
//! walk cost 27–128 s per read across the fleet, inside a 30 s deadline).
//!
//! What this catches: a wall read that stops snapshotting (the cache file never
//! appears), a resume that drops or duplicates posts across the snapshot seam, or
//! a corrupt snapshot served instead of rebuilt.

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
        .expect("wall read")
        .into_iter()
        .map(|post| post.body)
        .collect();
    bodies.sort();
    bodies
}

#[tokio::test]
async fn the_wall_snapshots_and_resumes_across_its_cursor_without_gap_or_dup() {
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;
    let room = alice.join("wall-room").await.expect("join");

    publish(&alice, "A").await;
    publish(&alice, "B").await;
    assert_eq!(
        bodies(&alice, &room).await,
        vec!["A".to_string(), "B".to_string()]
    );

    let snapshot = alice
        .home()
        .join("wall-cache")
        .join(format!("{}.json", room.channel));
    assert!(
        snapshot.is_file(),
        "the first read snapshots the wall at {}",
        snapshot.display()
    );
    let first = std::fs::read(&snapshot).expect("snapshot bytes");

    // A later post lands strictly after the snapshot cursor: the resume folds
    // it in (no gap) without re-adding the earlier ones (no dup), and the
    // snapshot advances.
    publish(&alice, "C").await;
    assert_eq!(
        bodies(&alice, &room).await,
        vec!["A".to_string(), "B".to_string(), "C".to_string()]
    );
    assert_ne!(
        std::fs::read(&snapshot).expect("snapshot bytes"),
        first,
        "the snapshot advanced"
    );

    // A corrupt snapshot is discarded and rebuilt, never served.
    std::fs::write(&snapshot, b"{ not json").expect("corrupt");
    assert_eq!(
        bodies(&alice, &room).await,
        vec!["A".to_string(), "B".to_string(), "C".to_string()]
    );
    assert!(
        std::fs::read(&snapshot).expect("bytes").starts_with(b"{\""),
        "rebuilt snapshot persisted"
    );
}
