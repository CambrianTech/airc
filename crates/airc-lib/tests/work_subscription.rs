//! Integration: typed work-event subscription stream.
//!
//! Proves work card e1f8e2e0: agents subscribe via
//! `Airc::subscribe_work_events` + `WorkEventFilter` and get typed
//! `WorkEvent` values without parsing CLI prose.
//!
//! Alice creates a work card via the existing `Airc::create_work_card`
//! SDK call; Bob (separate scope sharing the wire) subscribes with a
//! peer filter and asserts the `WorkEvent::CardCreated` arrives
//! decoded. Then Bob runs `recent_work_events` and reads the same
//! event back from the persisted transcript.

use std::time::Duration;

use airc_lib::{Airc, CreateWorkCard, PeerSpec, Priority, RepoId, WorkEvent, WorkEventFilter};
use futures::stream::StreamExt;
use tempfile::TempDir;

#[tokio::test]
async fn subscribe_work_events_yields_typed_card_created_event() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice.add_peer(bob_spec).await.expect("trust");
    bob.add_peer(alice_spec.clone()).await.expect("trust");

    alice
        .join_with_wire("work-subscription-test", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("work-subscription-test", wire_path)
        .await
        .expect("bob joins");

    // Bob subscribes BEFORE alice emits, with a peer filter set to
    // alice — proves the filter actually applies (not just "first
    // event wins").
    let filter = WorkEventFilter::new().with_peer(alice_spec.peer_id);
    let mut stream = Box::pin(bob.subscribe_work_events(filter).await.expect("subscribe"));

    // Tiny settle so bob's subscriber attaches.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let card_id = alice
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("test-org/test-repo").unwrap(),
            title: "test card".to_string(),
            body: Some("subscription proof".to_string()),
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .expect("alice creates card");

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut got = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some((_transcript_event, work_event))) => {
                if let WorkEvent::CardCreated(payload) = &work_event {
                    if payload.card_id == card_id {
                        got = Some(work_event);
                        break;
                    }
                }
            }
            Ok(None) => panic!("subscription closed before our event"),
            Err(_) => continue,
        }
    }

    let event = got.expect("CardCreated should arrive on the subscription");
    match event {
        WorkEvent::CardCreated(payload) => {
            assert_eq!(payload.card_id, card_id);
            assert_eq!(payload.created_by, alice_spec.peer_id);
        }
        other => panic!("expected CardCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn recent_work_events_reads_back_from_transcript() {
    let alice_home = TempDir::new().expect("alice home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    alice
        .join_with_wire("work-recent-test", wire_path)
        .await
        .expect("alice joins");

    let card_id = alice
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("test-org/test-repo").unwrap(),
            title: "recent test card".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
        })
        .await
        .expect("create card");

    // Give the wire subscriber a moment to ingest.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = alice
        .recent_work_events(WorkEventFilter::default(), 64)
        .await
        .expect("recent query");

    let found = events.iter().any(|event| match event {
        WorkEvent::CardCreated(payload) => payload.card_id == card_id,
        _ => false,
    });
    assert!(
        found,
        "recent_work_events should include the CardCreated we just emitted; got {events:?}"
    );
}

#[tokio::test]
async fn peer_filter_excludes_other_peers_events() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice.add_peer(bob_spec.clone()).await.expect("trust");
    bob.add_peer(alice_spec).await.expect("trust");

    alice
        .join_with_wire("work-peer-filter-test", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("work-peer-filter-test", wire_path)
        .await
        .expect("bob joins");

    // Alice creates a card. Filter scoped to Bob's peer id — should
    // exclude Alice's event entirely.
    alice
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("test-org/test-repo").unwrap(),
            title: "alice card".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
        })
        .await
        .expect("alice creates card");

    tokio::time::sleep(Duration::from_millis(150)).await;

    let bob_only = WorkEventFilter::new().with_peer(bob_spec.peer_id);
    let events = alice
        .recent_work_events(bob_only, 64)
        .await
        .expect("recent query");

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, WorkEvent::CardCreated(_))),
        "filter scoped to bob should not surface alice's CardCreated; got {events:?}"
    );
}
