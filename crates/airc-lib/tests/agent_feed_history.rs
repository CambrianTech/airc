//! Both SDK backends exclude typed historical chunks before the matching limit.
mod common;
use airc_core::{Body, EventId, Headers, MentionTarget, TranscriptCursor};
use airc_lib::{Airc, StreamChunk, HEADER_STREAM_ID};
use airc_protocol::FrameKind;
use futures::StreamExt;
use std::time::Duration;
fn position(c: &TranscriptCursor) -> (u64, u128) {
    (c.lamport, c.event_id.0.as_u128())
}
async fn chunk(airc: &Airc, n: usize) {
    let mut h = Headers::new();
    h.insert(HEADER_STREAM_ID.into(), format!("legacy-{n}"));
    airc.send_frame_to_for_test(FrameKind::Event, MentionTarget::All, Body::text("token"), h)
        .await
        .unwrap();
}
async fn exercise(airc: &Airc) {
    airc.join("a").await.unwrap();
    airc.join("b").await.unwrap();
    let start = airc.latest_subscribed_cursor().await.unwrap().unwrap();
    let mut expected: Vec<EventId> = vec![];
    for room in ["a", "b"] {
        airc.join(room).await.unwrap();
        for n in 0..80 {
            chunk(airc, n).await;
        }
        expected.push(airc.say("same text 🌍").await.unwrap());
        expected.push(
            airc.send_frame_to_for_test(
                FrameKind::Event,
                MentionTarget::All,
                Body::text("System warning"),
                Headers::new(),
            )
            .await
            .unwrap(),
        );
        for n in 80..96 {
            chunk(airc, n).await;
        }
    }
    let through = airc.latest_subscribed_cursor().await.unwrap().unwrap();
    let future = airc.say("after snapshot").await.unwrap();
    let mut at = start;
    let mut received = vec![];
    for _ in 0..32 {
        let page = airc.scan_agent_feed(Some(&at), &through, 2).await.unwrap();
        assert!(page.events.len() <= 2);
        for e in page.events {
            assert!(!e.headers.contains_key(HEADER_STREAM_ID));
            assert_ne!(e.event_id, future);
            assert!(position(&e.cursor()) > position(&at));
            assert!(!received.contains(&e.event_id));
            received.push(e.event_id);
        }
        let next = page.scanned_through.unwrap();
        assert!(position(&next) > position(&at));
        at = next;
        if at == through {
            break;
        }
    }
    assert_eq!(at, through);
    assert!(expected.iter().all(|id| received.contains(id)));
    let tail = airc.scan_agent_feed(None, &through, 2).await.unwrap();
    assert_eq!(
        tail.events.iter().map(|e| e.event_id).collect::<Vec<_>>(),
        expected[2..]
    );
    assert_eq!(tail.scanned_through, Some(through.clone()));
    assert!(airc
        .page_recent(100)
        .await
        .unwrap()
        .iter()
        .any(|e| e.headers.contains_key(HEADER_STREAM_ID)));
    let tip = airc.latest_subscribed_cursor().await.unwrap().unwrap();
    assert!(airc
        .scan_agent_feed(Some(&through), &tip, 10)
        .await
        .unwrap()
        .events
        .iter()
        .any(|e| e.event_id == future));
    let mut live = airc.subscribe_agent_feed().await.unwrap();
    airc.publish_stream_chunk(&StreamChunk::text_token("live", 0, "part"))
        .await
        .unwrap();
    let settled = airc.say("settled").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = live.next().await.unwrap().unwrap();
            assert!(!e.headers.contains_key(HEADER_STREAM_ID));
            if e.event_id == settled {
                break;
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn daemon_scan_preserves_messages_system_and_snapshot_across_rooms() {
    let machine = common::Machine::boot().await;
    machine.pin_identity("feed-regression").await;
    exercise(&machine.attach("agent").await).await;
}
#[tokio::test]
async fn embedded_scan_has_identical_filter_and_progress_semantics() {
    let root = tempfile::TempDir::new().unwrap();
    let store = airc_store::SqliteEventStore::open_path(&root.path().join("events.sqlite"))
        .await
        .unwrap();
    common::pin_identity(&store, "feed-regression").await;
    let airc = Airc::open_with_wire_root_for_test(root.path().join("agent"), root.path())
        .await
        .unwrap();
    exercise(&airc).await;
}
