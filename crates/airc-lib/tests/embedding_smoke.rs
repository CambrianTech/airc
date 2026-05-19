//! Embedding smoke test — proves a small consumer can link the lib
//! and exercise the Gate-4 minimum: open identity, join a room,
//! send, page_recent, resume_from, peer-spec/peers handling.
//!
//! No daemon involvement; the lib's in-process embedding is the
//! single linkage point. This is the slice-6 proof point.

use airc_lib::{Airc, Body, Headers, PeerSpec};
use tempfile::TempDir;

#[tokio::test]
async fn open_join_say_and_replay_round_trips_in_process() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();

    let room = airc.join("project-x").await.unwrap();
    assert_eq!(room.name, "project-x");
    let current = airc.current_room().await.unwrap();
    assert_eq!(current.channel, room.channel, "join persisted to room.json");

    // Direct send via Airc::say. The wire is the room's local-fs
    // path; the durable store is `<home>/events.sqlite`.
    let event_id = airc.say("hello, consumer").await.unwrap();
    let _ = event_id;

    // The wire-side write doesn't auto-populate the store in
    // pure-embedding mode (slice 6 deliberately ships without
    // background subscribers). Mirror the daemon's behaviour by
    // appending to the store explicitly so the proof shows the
    // consumer-facing `page_recent` path works end-to-end.
    let event = airc_lib::TranscriptEvent {
        event_id,
        room_id: room.channel,
        peer_id: airc.peer_id(),
        client_id: airc.client_id(),
        kind: airc_core::transcript::TranscriptKind::Message,
        occurred_at_ms: 1_700_000_000_000,
        lamport: 1,
        target: airc_lib::MentionTarget::All,
        headers: Headers::new(),
        body: Some(Body::text("hello, consumer")),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    };
    airc.append_event(event.clone()).await.unwrap();

    let page = airc.page_recent(10).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0], event);
    let bodies: Vec<&str> = page
        .iter()
        .filter_map(|e| e.body.as_ref().and_then(Body::as_text))
        .collect();
    assert_eq!(bodies, vec!["hello, consumer"]);

    let cursor = airc.latest_cursor().await.unwrap().unwrap();
    let after = airc.resume_from(&cursor, 10).await.unwrap();
    assert!(after.is_empty(), "nothing strictly after the latest cursor");
}

#[tokio::test]
async fn peer_spec_round_trips_via_add_peer() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();
    let alice = Airc::open(home_a.path()).await.unwrap();
    let bob = Airc::open(home_b.path()).await.unwrap();

    // Alice prints her spec; Bob enrols it; Bob's peers list now
    // includes Alice.
    let alice_spec_str = alice.peer_spec();
    let alice_spec: PeerSpec = alice_spec_str.parse().unwrap();
    bob.add_peer(alice_spec).await.unwrap();

    let peers = bob.peers().await.unwrap();
    let alice_in_bobs_book = peers.iter().any(|p| p.peer_id == alice.peer_id());
    assert!(
        alice_in_bobs_book,
        "alice's peer_id must appear in bob's enrolled peers"
    );
}

#[tokio::test]
async fn open_is_idempotent_across_handles() {
    // Two Airc::open calls on the same home recover the same
    // identity (and the same DB without migration conflicts).
    let home = TempDir::new().unwrap();
    let first = Airc::open(home.path()).await.unwrap();
    let first_peer = first.peer_id();
    drop(first);
    let second = Airc::open(home.path()).await.unwrap();
    assert_eq!(second.peer_id(), first_peer);
}
