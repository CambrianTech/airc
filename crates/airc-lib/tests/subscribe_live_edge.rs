//! Card bf0b5790: a daemon-attached live subscription starts at the
//! live edge — it must NOT replay the transcript backlog as if it
//! were live traffic.
//!
//! `Airc::subscribe` / `subscribe_subscribed_filtered` document live
//! semantics ("events emitted after the call surface here; for
//! historical events, use `recent_work_events` / `page_recent`").
//! Before this pin, the SDK attached with `from: None, from_now:
//! false`, which the daemon interprets as "resume from the start of
//! the transcript" — so every `airc join` flooded its consumer with
//! days of history indistinguishable from live events.

mod common;

use std::time::Duration;

use airc_core::Body;
use common::Machine;
use futures::stream::StreamExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_starts_at_live_edge_not_transcript_start() {
    let machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("live-edge-room").await;

    // History that exists BEFORE bob subscribes. If the subscription
    // replays the transcript, these are what arrive first.
    alice.say("history-1").await.expect("alice says history-1");
    alice.say("history-2").await.expect("alice says history-2");

    let mut bob_stream = bob.subscribe().await.expect("bob subscribes");

    // The first live event after the subscription.
    alice.say("live-1").await.expect("alice says live-1");

    // Drain until the live marker arrives; anything textual seen on
    // the way that predates the subscription is a backlog replay.
    let mut seen_before_live: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut got_live = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), bob_stream.next()).await {
            Ok(Some(Ok(event))) => {
                if let Some(text) = event.body.as_ref().and_then(Body::as_text) {
                    if text == "live-1" {
                        got_live = true;
                        break;
                    }
                    seen_before_live.push(text.to_string());
                }
            }
            Ok(Some(Err(_))) => {} // lag marker — keep reading
            Ok(None) => break,
            Err(_) => {} // poll timeout — keep waiting until deadline
        }
    }

    assert!(
        got_live,
        "the live event published after subscribing must arrive"
    );
    let replayed: Vec<&String> = seen_before_live
        .iter()
        .filter(|text| text.starts_with("history-"))
        .collect();
    assert!(
        replayed.is_empty(),
        "subscription replayed pre-subscribe backlog as live events: {replayed:?}"
    );
}
