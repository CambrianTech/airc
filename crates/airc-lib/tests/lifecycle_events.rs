//! Lifecycle event emit + subscribe round-trip.
//!
//! Phase 2 proof point: a consumer can subscribe to the substrate
//! stream and receive substrate-authored lifecycle events
//! (RoomJoined, etc.) without polling state files. Future emit
//! points (PeerArrived, WireEstablished, ...) extend this same
//! pattern.

use std::time::Duration;

use airc_core::{Body, TranscriptKind};
use airc_lib::{lifecycle::RoomJoinedBody, Airc};
use futures::stream::StreamExt;
use tempfile::TempDir;

#[tokio::test]
async fn join_emits_room_joined_lifecycle_event() {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");

    // Attach BEFORE join so we don't miss the lifecycle event the
    // join produces. The broadcast channel does have a small buffer
    // for in-flight delivery but a clean test asserts the stream
    // sees the live emit.
    let mut stream = airc.subscribe().await.expect("subscribe");

    let join_task = {
        let airc = airc.clone();
        tokio::spawn(async move {
            // Tiny sleep to let the subscribe attach fully.
            tokio::time::sleep(Duration::from_millis(20)).await;
            airc.join("lifecycle-test").await.expect("join")
        })
    };

    // Wait up to 2s for the RoomJoined event to surface on the
    // stream. The test fails loudly if it doesn't — the emit point
    // wired in airc.rs::join must produce this event.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut found = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if event.kind == TranscriptKind::RoomJoined {
                    found = Some(event);
                    break;
                }
            }
            Ok(Some(Err(_))) => continue,
            Ok(None) => panic!("stream closed before RoomJoined arrived"),
            Err(_) => continue,
        }
    }
    let room = join_task.await.expect("join completes");

    let event = found.expect("RoomJoined should have been emitted by airc.join");
    assert_eq!(event.kind, TranscriptKind::RoomJoined);
    let body = event.body.as_ref().expect("event has body");
    let body_json = match body {
        Body::Json(value) => value.clone(),
        _ => panic!("RoomJoined body should be JSON"),
    };
    let parsed: RoomJoinedBody =
        serde_json::from_value(body_json).expect("body parses as RoomJoinedBody");
    assert_eq!(parsed.channel_name, "lifecycle-test");
    assert_eq!(parsed.room_id, room.channel);
    assert!(parsed.is_default, "join sets the channel as default");
}

#[tokio::test]
async fn lifecycle_event_is_persisted_for_cursor_replay() {
    // Lifecycle events must be durable so a consumer that reconnects
    // can replay the transitions it missed. Confirm by paging
    // recent events after join.
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");

    let _room = airc.join("durability-test").await.expect("join");

    // page_recent reads from the store; if the lifecycle event was
    // persisted, it shows up here.
    let page = airc.page_recent(16).await.expect("page");
    let lifecycle_count = page
        .iter()
        .filter(|e| e.kind == TranscriptKind::RoomJoined)
        .count();
    assert!(
        lifecycle_count >= 1,
        "at least one RoomJoined event should be persisted: {lifecycle_count} found in page of {} events",
        page.len()
    );
}
