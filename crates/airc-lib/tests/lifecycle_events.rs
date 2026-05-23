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
async fn add_peer_emits_peer_arrived_with_manual_via() {
    use airc_core::PeerId;
    use airc_lib::lifecycle::PeerArrivedBody;
    use airc_lib::PeerSpec;

    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");
    let _room = airc.join("peer-arrived-test").await.expect("join");

    let mut stream = airc.subscribe().await.expect("subscribe");

    let new_peer = PeerSpec {
        peer_id: PeerId::new(),
        pubkey: [9u8; 32],
    };
    let expected_peer_id = new_peer.peer_id;

    let add_task = {
        let airc = airc.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            airc.add_peer(new_peer).await.expect("add_peer");
        })
    };

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut found = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if event.kind == TranscriptKind::PeerArrived {
                    found = Some(event);
                    break;
                }
            }
            Ok(Some(Err(_))) => continue,
            Ok(None) => panic!("stream closed before PeerArrived arrived"),
            Err(_) => continue,
        }
    }
    add_task.await.expect("add completes");

    let event = found.expect("PeerArrived should be emitted by add_peer");
    let body = event.body.as_ref().expect("event has body");
    let body_json = match body {
        Body::Json(value) => value.clone(),
        _ => panic!("PeerArrived body should be JSON"),
    };
    let parsed: PeerArrivedBody =
        serde_json::from_value(body_json).expect("body parses as PeerArrivedBody");
    assert_eq!(parsed.peer_id, expected_peer_id);
    assert_eq!(
        parsed.via, "manual",
        "default add_peer path tags via=manual"
    );
}

#[tokio::test]
async fn add_peer_is_idempotent_no_duplicate_lifecycle_event() {
    use airc_core::PeerId;
    use airc_lib::PeerSpec;

    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");
    airc.join("dedup-test").await.expect("join");

    let spec = PeerSpec {
        peer_id: PeerId::new(),
        pubkey: [3u8; 32],
    };
    airc.add_peer(spec.clone()).await.expect("first add");
    airc.add_peer(spec).await.expect("second add (idempotent)");

    let page = airc.page_recent(64).await.expect("page");
    let count = page
        .iter()
        .filter(|e| e.kind == TranscriptKind::PeerArrived)
        .count();
    assert_eq!(
        count, 1,
        "re-adding an already-known peer must not emit a duplicate PeerArrived"
    );
}

#[tokio::test]
async fn join_emits_wire_established_after_subscriber_attaches() {
    use airc_lib::lifecycle::WireEstablishedBody;

    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");

    // join() drives ensure_wire_subscriber which emits the event.
    let _room = airc.join("wire-established-test").await.expect("join");

    // Page recent events; both RoomJoined and WireEstablished
    // should land. The order between them is implementation
    // detail (current order: RoomJoined fires inside join() after
    // ensure_wire_subscriber returns, so WireEstablished is first
    // in the lamport sequence — but the test asserts presence,
    // not order).
    let page = airc.page_recent(64).await.expect("page");
    let wire_event = page
        .iter()
        .find(|e| e.kind == TranscriptKind::WireEstablished)
        .expect("WireEstablished should be emitted when the wire subscriber attaches");
    let body = wire_event.body.as_ref().expect("event has body");
    let body_json = match body {
        Body::Json(value) => value.clone(),
        _ => panic!("WireEstablished body should be JSON"),
    };
    let parsed: WireEstablishedBody =
        serde_json::from_value(body_json).expect("body parses as WireEstablishedBody");
    assert_eq!(parsed.channel_name, "wire-established-test");
    assert!(!parsed.wire.is_empty(), "wire path should be set");
}

#[tokio::test]
async fn wire_established_fires_once_per_wire_idempotent() {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");

    let _room = airc.join("idempotent-wire").await.expect("first join");
    // Second join() to the same channel — should hit the
    // contains_key short-circuit in ensure_wire_subscriber. No
    // duplicate WireEstablished.
    let _room2 = airc.join("idempotent-wire").await.expect("second join");

    let page = airc.page_recent(64).await.expect("page");
    let count = page
        .iter()
        .filter(|e| e.kind == TranscriptKind::WireEstablished)
        .count();
    assert_eq!(
        count, 1,
        "ensure_wire_subscriber must short-circuit on the second call; only one WireEstablished expected"
    );
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
