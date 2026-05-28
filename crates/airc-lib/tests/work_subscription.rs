//! Integration: typed work-event subscription stream, over the daemon.
//!
//! Proves work card e1f8e2e0: agents subscribe via
//! `Airc::subscribe_work_events` + `WorkEventFilter` and get typed
//! `WorkEvent` values without parsing CLI prose.
//!
//! Alice creates a work card via `Airc::create_work_card`; Bob —
//! another scope on the same machine, attached to the one owner-core
//! daemon — subscribes with a peer filter and asserts the typed
//! `WorkEvent::CardCreated` arrives. Then `recent_work_events` reads
//! the same event back from the daemon's durable transcript.

mod common;

use std::time::Duration;

use airc_lib::{CreateWorkCard, Priority, RepoId, WorkEvent, WorkEventFilter};
use common::Machine;
use futures::stream::StreamExt;

#[tokio::test]
async fn subscribe_work_events_yields_typed_card_created_event() {
    let machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("work-subscription-test").await;
    let alice_peer = alice.peer_id();

    // Bob subscribes BEFORE alice emits, with a peer filter set to
    // Alice — proves per-agent attribution survives the broker: the
    // daemon stamps Alice's participant identity, so a peer-scoped
    // filter admits her events (and would exclude anyone else's).
    let filter = WorkEventFilter::new().with_peer(alice_peer);
    let mut stream = Box::pin(bob.subscribe_work_events(filter).await.expect("subscribe"));

    // Tiny settle so bob's daemon attach is live.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let card_id = alice
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("test-org/test-repo").unwrap(),
            title: "test card".to_string(),
            body: Some("subscription proof".to_string()),
            priority: Priority::P1,
            lane_id: None,
            reviews: None,
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
            assert_eq!(payload.created_by, alice_peer);
        }
        other => panic!("expected CardCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn recent_work_events_reads_back_from_transcript() {
    let machine = Machine::boot().await;
    let alice = machine.solo("work-recent-test").await;

    let card_id = alice
        .create_work_card(CreateWorkCard {
            repo: RepoId::new("test-org/test-repo").unwrap(),
            title: "recent test card".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            reviews: None,
        })
        .await
        .expect("create card");

    // Give the daemon a moment to durably record the card event.
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
