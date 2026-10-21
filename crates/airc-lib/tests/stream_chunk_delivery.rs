//! Real publisher regression: chunks remain live, while only settled text is history.
mod common;

use std::time::Duration;

use airc_core::EventId;
use airc_lib::{Airc, StreamChunk};
use airc_protocol::headers_keys::HEADER_AIRC_DELIVERY_CLASS;
use airc_store::SqliteEventStore;
use futures::StreamExt;

async fn assert_history(airc: &Airc, chunks: &[EventId], settled: EventId) {
    let history = airc.page_recent(100).await.expect("read history");
    assert!(history.iter().any(|event| event.event_id == settled));
    assert!(
        history
            .iter()
            .all(|event| !chunks.contains(&event.event_id)),
        "published stream fragments must not become durable transcript rows"
    );
}

async fn exercise_publisher(airc: &Airc) -> (Vec<EventId>, EventId) {
    let mut live = airc.subscribe().await.expect("subscribe before publishing");
    let mut chunks = Vec::new();
    for chunk in [
        StreamChunk::text_token("delivery-contract", 0, "hello 🌍"),
        StreamChunk::text_end("delivery-contract", 1),
    ] {
        chunks.push(
            airc.publish_stream_chunk(&chunk)
                .await
                .expect("publish chunk"),
        );
    }
    let settled = airc.say("hello 🌍").await.expect("publish settled message");
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut seen = Vec::new();
        while seen.len() < chunks.len() {
            let event = live
                .next()
                .await
                .expect("live stream open")
                .expect("no lag");
            if chunks.contains(&event.event_id) && !seen.contains(&event.event_id) {
                assert_eq!(
                    event
                        .headers
                        .get(HEADER_AIRC_DELIVERY_CLASS)
                        .map(String::as_str),
                    Some("stream_chunk")
                );
                seen.push(event.event_id);
            }
        }
    })
    .await
    .expect("every chunk, including the final marker, remains available live");
    assert_history(airc, &chunks, settled).await;
    (chunks, settled)
}

#[tokio::test]
async fn embedded_publisher_keeps_chunks_live_without_persisting_them() {
    let root = tempfile::TempDir::new().expect("isolated root");
    let store = SqliteEventStore::open_path(&root.path().join("events.sqlite"))
        .await
        .expect("coordinator store");
    common::pin_identity(&store, "stream-producer-contract").await;
    let airc = Airc::open_with_wire_root_for_test(root.path().join("scope"), root.path())
        .await
        .expect("open embedded SDK");
    airc.join("stream-contract").await.expect("join room");
    exercise_publisher(&airc).await;
}

#[tokio::test]
async fn daemon_publisher_keeps_chunks_live_and_out_of_restarted_history() {
    let mut machine = common::Machine::boot().await;
    machine.pin_identity("stream-producer-contract").await;
    let airc = machine.solo("stream-contract").await;
    let (chunks, settled) = exercise_publisher(&airc).await;
    drop(airc);
    machine.restart_daemon().await;
    let reopened = machine.attach("solo").await;
    assert_history(&reopened, &chunks, settled).await;
}
