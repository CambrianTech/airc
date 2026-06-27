//! End-to-end proof: two `Airc` consumers, each in its own `home`,
//! attach to the one machine daemon, exchange messages through the SDK,
//! and replay from the transcript — without the CLI and without
//! reaching into substrate internals (the runtime surface is airc-lib
//! only; the test provides the in-process daemon the install ships).
//!
//! This is the Gate-4 minimum: a downstream consumer linking only
//! `airc-lib` can stand up an agent, hold a conversation, and walk
//! history via the typed cursor API.

mod common;

use std::time::Duration;

use airc_lib::{Airc, Body, TranscriptCursor};
use common::Machine;
use futures::stream::StreamExt;

/// Poll `page_recent` until the expected body text shows up, with a
/// deadline — keeps slow CI runners from flaking while still failing
/// loudly past the timeout.
async fn wait_for_body_text(
    airc: &Airc,
    expected: &str,
    timeout: Duration,
) -> airc_lib::TranscriptEvent {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let page = airc.page_recent(64).await.unwrap();
        if let Some(event) = page
            .into_iter()
            .find(|event| event.body.as_ref().and_then(Body::as_text) == Some(expected))
        {
            return event;
        }
        if std::time::Instant::now() >= deadline {
            panic!("wait_for_body_text: never saw {expected:?} within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_agents_in_separate_homes_exchange_and_replay_via_sdk() {
    // One machine daemon; two consumers, each its own identity home,
    // attached to it. A real embedder gives every agent its own home;
    // the shared machine daemon is what carries same-machine delivery.
    let machine = Machine::boot().await;
    let alice = machine.attach("alice").await;
    let bob = machine.attach("bob").await;

    // Trust bootstrap: each consumer enrols the other's pubkey before
    // its verifier will accept the other's signed frames (audit §8 —
    // no silent acceptance). Real embedders wire this from an
    // out-of-band trust source (invite, paired QR, signed config).
    common::trust(&alice, &bob).await;

    let room = "embedded-smoke";
    alice.join(room).await.unwrap();
    bob.join(room).await.unwrap();

    // Bob installs a live subscription BEFORE Alice sends, so the
    // "no prompt-time polling" path is exercised: a consumer subscribes
    // to typed events and receives them as they happen, instead of
    // shelling out to a log-scraping command between turns.
    let mut bob_stream = bob.subscribe().await.unwrap();

    let first = "hello bob, from alice via sdk";
    alice.say(first).await.unwrap();

    // Live receive.
    let live = tokio::time::timeout(Duration::from_secs(3), bob_stream.next())
        .await
        .expect("bob's subscribe stream did not produce a live event within 3s")
        .expect("bob's subscribe stream closed before delivery")
        .expect("bob received a transcript error");
    assert_eq!(
        live.body.as_ref().and_then(Body::as_text),
        Some(first),
        "live subscribe must deliver the alice→bob text exactly",
    );

    // Durable replay from the daemon transcript.
    let recent = wait_for_body_text(&bob, first, Duration::from_secs(3)).await;
    assert_eq!(recent.event_id, live.event_id);

    // Cursor-based catch-up. A real embedder restarts on its own
    // schedule and uses the cursor to resume without re-receiving
    // events it already processed. The cursor we capture here MUST
    // exclude `first` from the resume page and include the next message.
    let cursor_after_first = TranscriptCursor {
        lamport: live.lamport,
        event_id: live.event_id,
    };

    let second = "second message — proves resume_from excludes prior cursor";
    alice.say(second).await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let after_cursor = loop {
        let page = bob.resume_from(&cursor_after_first, 64).await.unwrap();
        if page
            .iter()
            .any(|event| event.body.as_ref().and_then(Body::as_text) == Some(second))
        {
            break page;
        }
        if std::time::Instant::now() >= deadline {
            panic!("resume_from never returned {second:?} within 3s");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    assert!(
        after_cursor
            .iter()
            .all(|event| event.body.as_ref().and_then(Body::as_text) != Some(first)),
        "resume_from cursor must EXCLUDE the message at the cursor itself; \
         got page that still contains {first:?}: {after_cursor:?}",
    );
}
