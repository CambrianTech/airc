//! Reliability: a live daemon-attached subscription survives a daemon
//! restart — it reconnects and RESUMES strictly after its last cursor,
//! so durable events published while it was reconnecting still arrive.
//!
//! This is the gap `daemon_lifecycle` (CLI) left open: one-shot commands
//! respawn the daemon fine, but a long-lived stream (monitor, codex
//! hook, Continuum) used to go permanently deaf on a daemon bounce.

mod common;

use std::sync::Arc;
use std::time::Duration;

use airc_core::{TranscriptEvent, TranscriptKind};
use airc_lib::EventStream;
use common::Machine;
use futures::stream::StreamExt;

async fn next_message(stream: &mut EventStream, timeout: Duration) -> Arc<TranscriptEvent> {
    tokio::time::timeout(timeout, async {
        loop {
            let event = stream
                .next()
                .await
                .expect("subscription remains open")
                .expect("subscription did not lag");
            if event.kind == TranscriptKind::Message {
                return event;
            }
        }
    })
    .await
    .expect("next message arrives before the deadline")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_subscription_survives_daemon_restart_and_resumes_durable_gap() {
    let mut machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("reconnect-room").await;

    // Bob holds a live subscription across the whole test.
    let mut bob_stream = bob.subscribe().await.expect("bob subscribes");

    // Baseline: delivery works before any restart.
    let baseline = alice.say("before-restart").await.expect("alice says");
    let mut consumed = next_message(&mut bob_stream, Duration::from_secs(3)).await;
    assert_eq!(consumed.event_id, baseline);
    let consumer = "join-feed:restart-reader";
    bob.save_runtime_cursor_for_event(consumer, &consumed)
        .await
        .expect("checkpoint the consumed baseline");

    for text in ["after-restart", "after-second-restart"] {
        // The same owner store and scope survive. A reopened consumer uses
        // its named durable bookmark; the still-live stream keeps its own
        // IPC cursor. Neither may re-notify already-consumed messages.
        machine.restart_daemon().await;
        let reopened = machine.attach("bob").await;
        let saved = reopened
            .load_runtime_cursor(consumer)
            .await
            .expect("load durable consumer bookmark")
            .expect("bookmark survives restart");
        assert_eq!(saved, consumed.cursor());

        let fresh = alice.say(text).await.expect("alice says after restart");
        consumed = next_message(&mut bob_stream, Duration::from_secs(10)).await;
        assert_eq!(
            consumed.event_id, fresh,
            "the next live message must be new, never an old replay"
        );
        let unread: Vec<_> = reopened
            .resume_from(&saved, 16)
            .await
            .expect("resume cold consumer from its bookmark")
            .into_iter()
            .filter(|event| event.kind == TranscriptKind::Message)
            .map(|event| event.event_id)
            .collect();
        assert_eq!(unread, vec![fresh]);
        reopened
            .save_runtime_cursor_for_event(consumer, &consumed)
            .await
            .expect("advance after processing the recovered event");
    }
}
