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
async fn remove_peer_emits_peer_departed_lifecycle_event() {
    use airc_core::PeerId;
    use airc_lib::lifecycle::PeerDepartedBody;
    use airc_lib::PeerSpec;
    use airc_protocol::PeerKeypair;

    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");
    let _room = airc.join("peer-departed-test").await.expect("join");

    let peer_id = PeerId::new();
    let peer_keypair = PeerKeypair::generate();
    airc.add_peer(PeerSpec {
        peer_id,
        pubkey: peer_keypair.public_bytes(),
    })
    .await
    .expect("add peer");

    let mut stream = airc.subscribe().await.expect("subscribe");
    let removed = airc
        .remove_peer(peer_id, "manual")
        .await
        .expect("remove peer");
    assert!(removed, "known peer should be removed");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut found = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if event.kind == TranscriptKind::PeerDeparted {
                    found = Some(event);
                    break;
                }
            }
            Ok(Some(Err(_))) => continue,
            Ok(None) => panic!("stream closed before PeerDeparted arrived"),
            Err(_) => continue,
        }
    }

    let event = found.expect("PeerDeparted lifecycle event exists");
    let body = event.body.as_ref().expect("event has body");
    let body_json = match body {
        Body::Json(value) => value.clone(),
        _ => panic!("PeerDeparted body should be JSON"),
    };
    let parsed: PeerDepartedBody =
        serde_json::from_value(body_json).expect("body parses as PeerDepartedBody");
    assert_eq!(parsed.peer_id, peer_id);
    assert_eq!(parsed.reason, "manual");
    assert!(
        !airc
            .peers()
            .await
            .expect("peers")
            .iter()
            .any(|peer| peer.peer_id == peer_id),
        "removed peer must not remain in durable peer list"
    );
}

#[tokio::test]
async fn save_runtime_cursor_emits_subscription_advanced() {
    use airc_lib::lifecycle::SubscriptionAdvancedBody;

    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");
    let _room = airc.join("subscription-advanced-test").await.expect("join");

    // Runtime cursors are local consumer state; bookmark the join's
    // own RoomJoined lifecycle event as the source — a local transcript
    // event, no network route needed.
    let source = airc
        .page_recent(32)
        .await
        .expect("page")
        .into_iter()
        .find(|event| event.kind == TranscriptKind::RoomJoined)
        .expect("RoomJoined event exists");
    let source_cursor = source.cursor();

    airc.save_runtime_cursor_for_event("test-consumer", &source)
        .await
        .expect("save cursor");

    assert_eq!(
        airc.load_runtime_cursor("test-consumer").await.unwrap(),
        Some(source_cursor.clone())
    );

    let page = airc.page_recent(64).await.expect("page");
    let event = page
        .iter()
        .find(|event| event.kind == TranscriptKind::SubscriptionAdvanced)
        .expect("SubscriptionAdvanced lifecycle event exists");
    let body = event.body.as_ref().expect("event has body");
    let body_json = match body {
        Body::Json(value) => value.clone(),
        _ => panic!("SubscriptionAdvanced body should be JSON"),
    };
    let parsed: SubscriptionAdvancedBody =
        serde_json::from_value(body_json).expect("body parses as SubscriptionAdvancedBody");
    assert_eq!(parsed.consumer_id, "test-consumer");
    assert_eq!(parsed.lamport, source_cursor.lamport);
    assert_eq!(parsed.event_id, source_cursor.event_id);
}

#[tokio::test]
async fn part_channel_emits_room_parted_lifecycle_event() {
    use airc_lib::lifecycle::RoomPartedBody;

    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");

    let room = airc.join("part-me").await.expect("join");
    let mut stream = airc.subscribe().await.expect("subscribe");
    let parted = airc
        .part_channel(Some("part-me"))
        .await
        .expect("part channel");
    assert_eq!(parted.channel, room.channel);
    assert!(
        !airc
            .is_subscribed(&airc_lib::ChannelName::new("part-me").unwrap())
            .await
            .expect("subscription query"),
        "parted channel must no longer be subscribed"
    );

    let mut found = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if event.kind == TranscriptKind::RoomParted {
                    found = Some(event);
                    break;
                }
            }
            Ok(Some(Err(_))) => continue,
            Ok(None) => panic!("stream closed before RoomParted arrived"),
            Err(_) => continue,
        }
    }
    let event = found.expect("RoomParted lifecycle event exists");
    assert_eq!(event.room_id, room.channel);
    let body = event.body.as_ref().expect("event has body");
    let body_json = match body {
        Body::Json(value) => value.clone(),
        _ => panic!("RoomParted body should be JSON"),
    };
    let parsed: RoomPartedBody =
        serde_json::from_value(body_json).expect("body parses as RoomPartedBody");
    assert_eq!(parsed.channel_name, "part-me");
    assert_eq!(parsed.room_id, room.channel);
}

#[tokio::test]
async fn saving_subscription_advanced_cursor_does_not_emit_recursive_event() {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path().join(".airc");
    std::fs::create_dir_all(&home).unwrap();
    let airc = Airc::open(&home).await.expect("open");
    let _room = airc
        .join("subscription-advanced-loop-test")
        .await
        .expect("join");

    let source = airc
        .page_recent(32)
        .await
        .expect("page")
        .into_iter()
        .find(|event| event.kind == TranscriptKind::RoomJoined)
        .expect("RoomJoined event exists");
    airc.save_runtime_cursor_for_event("test-consumer", &source)
        .await
        .expect("save source cursor");

    let lifecycle_event = airc
        .page_recent(64)
        .await
        .expect("page")
        .into_iter()
        .find(|event| event.kind == TranscriptKind::SubscriptionAdvanced)
        .expect("SubscriptionAdvanced lifecycle event exists");
    airc.save_runtime_cursor_for_event("test-consumer", &lifecycle_event)
        .await
        .expect("save lifecycle cursor");

    let page = airc.page_recent(128).await.expect("page");
    let count = page
        .iter()
        .filter(|event| event.kind == TranscriptKind::SubscriptionAdvanced)
        .count();
    assert_eq!(
        count, 1,
        "saving a SubscriptionAdvanced cursor must not emit another SubscriptionAdvanced event"
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
