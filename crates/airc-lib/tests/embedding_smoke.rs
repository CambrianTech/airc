//! Embedding smoke test — proves a small consumer can link the lib
//! and exercise the Gate-4 minimum: open identity, join a room,
//! send, observe in store via `page_recent`, subscribe to live
//! events, fetch replay via `resume_from`, peer-spec/peers handling.
//!
//! No daemon involvement; the lib's in-process embedding owns the
//! background subscriber, the store, and the broadcast fan-out.
//! This is the slice-6b proof point.

use std::time::Duration;

use airc_lib::{
    Airc, Body, EventFilter, HeaderFilter, Headers, PeerSpec, TranscriptKind,
    TransportHealthSample, TransportKind, TransportRole,
};
use futures::stream::StreamExt;
use tempfile::TempDir;

/// Poll `page_recent` until it sees at least `expected` events or
/// the deadline fires. The wire-side tail loop runs in a background
/// task; first attaches replay from the start of the wire, but the
/// store append happens asynchronously after the local-fs adapter
/// observes the new line. A handful of polls keeps the test
/// deterministic without flaking on slow CI runners.
async fn wait_for_events(
    airc: &Airc,
    expected: usize,
    timeout: Duration,
) -> Vec<airc_lib::TranscriptEvent> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let page = airc.page_recent(64).await.unwrap();
        if page.len() >= expected {
            return page;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "wait_for_events: expected {expected} got {} in {:?}",
                page.len(),
                timeout
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn open_join_say_and_replay_round_trips_in_process() {
    // Gate-4 minimum: a consumer can link airc-lib, open the substrate,
    // join a room, send, and observe the event via the store — without
    // manually feeding the store. Before slice 6b this test had to
    // call `airc.append_event(...)` because `say` only wrote to the
    // wire; the background subscriber now closes that loop.
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();

    let room = airc.join("project-x").await.unwrap();
    assert_eq!(room.name, "project-x");
    let current = airc.current_room().await.unwrap();
    assert_eq!(current.channel, room.channel, "join persisted to room.json");

    let _event_id = airc.say("hello, consumer").await.unwrap();

    let page = wait_for_events(&airc, 1, Duration::from_secs(2)).await;
    assert_eq!(page.len(), 1);
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
async fn subscribe_yields_live_events_in_order() {
    // The live subscription contract: subscribers see every event
    // the substrate appends to the store, in transcript order, while
    // the consumer is connected.
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("live-test").await.unwrap();

    let mut stream = airc.subscribe().await.unwrap();

    // Drive three sends from a spawned task. Cloning the Airc
    // handle is critical: a fresh `Airc::open` on the same home
    // would have its OWN broadcast channel, and the stream we're
    // holding wouldn't see fan-outs from that handle's subscriber.
    let airc_send = airc.clone();
    let send_task = tokio::spawn(async move {
        for i in 0..3 {
            airc_send.say(&format!("hi-{i}")).await.unwrap();
        }
    });

    let mut received = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while received.len() < 3 && std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if let Some(text) = event.body.as_ref().and_then(Body::as_text) {
                    if text.starts_with("hi-") {
                        received.push(text.to_string());
                    }
                }
            }
            Ok(Some(Err(lag))) => panic!("unexpected live-stream lag: {lag}"),
            Ok(None) => panic!("stream closed unexpectedly"),
            Err(_) => continue,
        }
    }
    send_task.await.unwrap();

    assert_eq!(
        received,
        vec!["hi-0", "hi-1", "hi-2"],
        "subscriber must observe all three sends in send order"
    );
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

#[tokio::test]
async fn send_typed_body_with_headers_round_trips() {
    // Gate-4 bullet: "send typed body with headers". The headers
    // survive the wire boundary and land in the persisted event.
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("typed-test").await.unwrap();

    let mut headers = Headers::new();
    headers.insert(
        "forge.body_hint".to_string(),
        "application/json".to_string(),
    );
    headers.insert("x-test-marker".to_string(), "round-trip".to_string());
    let _event_id = airc
        .send(Body::text(r#"{"k":"v"}"#), headers.clone())
        .await
        .unwrap();

    let page = wait_for_events(&airc, 1, Duration::from_secs(2)).await;
    assert_eq!(page.len(), 1);
    assert_eq!(
        page[0].headers.get("x-test-marker").map(String::as_str),
        Some("round-trip")
    );
    assert_eq!(
        page[0].headers.get("forge.body_hint").map(String::as_str),
        Some("application/json")
    );
}

#[tokio::test]
async fn send_refuses_github_invite_only_route() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("route-gate").await.unwrap();
    airc.replace_transport_health([TransportHealthSample {
        kind: TransportKind::GhGist,
        role: TransportRole::InviteBeacon,
        state: airc_lib::TransportHealthState::Healthy,
        rtt_ms: None,
        success_ppm: None,
    }])
    .unwrap();

    let err = airc.say("must not go through gist").await.unwrap_err();

    assert!(
        err.to_string()
            .contains("DataInteractive has no admissible live route"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn filtered_event_queries_match_kind_and_headers_without_body_parse() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("filtered-events").await.unwrap();

    let mut headers = Headers::new();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.persona.turn".to_string(),
    );
    headers.insert("continuum.activity".to_string(), "general".to_string());
    airc.send(Body::text("persona turn payload"), headers)
        .await
        .unwrap();
    airc.say("plain chat").await.unwrap();

    let mut filter = EventFilter::current_room();
    filter.kinds.insert(TranscriptKind::Message);
    filter.headers_filter = HeaderFilter::Prefix {
        key: "forge.body_hint".to_string(),
        value_prefix: "forge.persona.".to_string(),
    };

    let matches = airc.page_recent_filtered(filter, 32).await.unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].body.as_ref().and_then(Body::as_text),
        Some("persona turn payload")
    );
}
