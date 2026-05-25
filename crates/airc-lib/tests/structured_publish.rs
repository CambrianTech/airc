//! Integration: structured publish API routes typed bodies to
//! named rooms without touching the default-room pointer, and
//! returns a typed receipt instead of human-prose.
//!
//! Proves work card a0d740fa (P1): "Structured AIRC publish API
//! for Continuum chat dual-write". The receipt is the JSON shape
//! the CLI emits to stdout for shell consumers; the in-process
//! API is the linkable shape for Rust consumers like Continuum.

use std::time::Duration;

use airc_core::{Body, EventId, Headers};
use airc_lib::{Airc, PeerSpec, PublishTarget};
use airc_protocol::FrameKind;
use futures::stream::StreamExt;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn publish_to_room_by_name_does_not_mutate_default_pointer() {
    // Alice joins room-a then room-b. Joining a room sets it as
    // default, so after both joins the default is room-b. Then she
    // publishes BY NAME to room-a. The default must remain room-b —
    // publish must not change which room "current room" points at.
    let home = TempDir::new().expect("home");
    let airc = Airc::open(home.path()).await.expect("open");

    let wire_a = TempDir::new().expect("wire-a");
    let wire_b = TempDir::new().expect("wire-b");
    airc.join_with_wire("room-a", wire_a.path().join("wire.jsonl"))
        .await
        .expect("join room-a");
    airc.join_with_wire("room-b", wire_b.path().join("wire.jsonl"))
        .await
        .expect("join room-b");

    let before = airc.current_room().await.expect("current room before");
    assert_eq!(
        before.name, "room-b",
        "second join should be the default room"
    );

    let receipt = airc
        .publish(
            PublishTarget::RoomByName("room-a".into()),
            FrameKind::Event,
            Body::Json(json!({"kind":"chat","text":"hi"})),
            Headers::new(),
        )
        .await
        .expect("publish");

    assert_eq!(receipt.channel_name, "room-a");
    assert_ne!(receipt.event_id, EventId::from_uuid(uuid::Uuid::nil()));

    let after = airc.current_room().await.expect("current room after");
    assert_eq!(
        after.name, "room-b",
        "publishing to room-a must not mutate the default pointer"
    );
    assert_eq!(
        after.channel, before.channel,
        "current channel id should be unchanged"
    );
}

#[tokio::test]
async fn publish_refuses_unsubscribed_room_rather_than_auto_join() {
    // Publish is an intentional non-auto-join surface: trying to
    // route to a channel this scope has never joined must fail with
    // a clear error, not silently subscribe.
    let home = TempDir::new().expect("home");
    let airc = Airc::open(home.path()).await.expect("open");
    let wire = TempDir::new().expect("wire");
    airc.join_with_wire("only-room", wire.path().join("wire.jsonl"))
        .await
        .expect("join");

    let err = airc
        .publish(
            PublishTarget::RoomByName("never-joined".into()),
            FrameKind::Event,
            Body::text("should never land"),
            Headers::new(),
        )
        .await
        .expect_err("publish to unsubscribed room must fail");

    let message = err.to_string();
    assert!(
        message.contains("never-joined"),
        "error should name the channel, got: {message}"
    );
    assert!(
        message.contains("not subscribed") || message.contains("join the room first"),
        "error should explain refusal + remedy, got: {message}"
    );
}

#[tokio::test]
async fn publish_to_current_room_round_trips_to_a_subscriber() {
    // Alice publishes; Bob (sharing the wire) sees the frame with
    // matching channel and body. Receipt event id matches the
    // transcript event id the subscriber receives.
    let alice_home = TempDir::new().expect("alice");
    let bob_home = TempDir::new().expect("bob");
    let wire = TempDir::new().expect("wire");
    let wire_path = wire.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice open");
    let bob = Airc::open(bob_home.path()).await.expect("bob open");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    alice.add_peer(bob_spec).await.expect("trust bob");
    bob.add_peer(alice_spec).await.expect("trust alice");

    alice
        .join_with_wire("publish-roundtrip", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("publish-roundtrip", wire_path)
        .await
        .expect("bob joins");

    let mut subscription = bob.subscribe().await.expect("bob subscribes");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let body = json!({"kind":"chat","payload":{"text":"structured publish works"}});
    let mut headers = Headers::new();
    headers.insert("airc.continuum.kind".into(), "chat_transcript".into());

    let receipt = alice
        .publish(
            PublishTarget::CurrentRoom,
            FrameKind::Event,
            Body::Json(body.clone()),
            headers,
        )
        .await
        .expect("alice publishes");

    assert_eq!(receipt.channel_name, "publish-roundtrip");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut matched = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), subscription.next()).await {
            Ok(Some(Ok(event))) => {
                if event.event_id == receipt.event_id {
                    assert_eq!(event.room_id, receipt.channel_id);
                    assert_eq!(event.lamport, receipt.lamport);
                    assert_eq!(
                        event.headers.get("airc.continuum.kind").map(String::as_str),
                        Some("chat_transcript")
                    );
                    matched = true;
                    break;
                }
            }
            Ok(Some(Err(_))) | Ok(None) => continue,
            Err(_) => continue,
        }
    }
    assert!(matched, "subscriber did not see the published event");
}
