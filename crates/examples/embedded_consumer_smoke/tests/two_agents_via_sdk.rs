//! End-to-end proof: two `Airc` consumers, each in its own `home`,
//! exchange messages through the SDK and replay from the store —
//! without daemon IPC, without the CLI, and without reaching into
//! substrate internals.
//!
//! This is the Gate-4 minimum: a downstream consumer linking only
//! `airc-lib` can stand up an agent, hold a conversation, and walk
//! history from the typed store.

use std::time::Duration;

use airc_lib::{Airc, Body, PeerSpec, TranscriptCursor};
use futures::stream::StreamExt;
use tempfile::TempDir;

/// Poll `page_recent` until the expected body text shows up, with a
/// deadline. The local-fs wire flushes asynchronously; a deterministic
/// polling helper avoids flakes on slow CI runners while still
/// failing loudly past the timeout.
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

#[tokio::test]
async fn two_agents_in_separate_homes_exchange_and_replay_via_sdk() {
    // Two distinct identity homes. Each consumer maintains its own
    // PeerId, keypair, room state, and event store. A real embedder
    // gives every agent its own home; nothing about the SDK assumes
    // a single global home.
    let alice_home = TempDir::new().unwrap();
    let bob_home = TempDir::new().unwrap();

    // Shared wire — a local-fs append-only log both consumers can
    // attach to. This is the in-process equivalent of a relay or
    // LAN-TCP: a shared transport surface without daemon IPC.
    let wire_dir = TempDir::new().unwrap();
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.unwrap();
    let bob = Airc::open(bob_home.path()).await.unwrap();

    // Trust bootstrap: each consumer must enrol the other's pubkey
    // before its verifier will accept signed frames on the wire.
    // This is the "explicit trust" half of the audit's grievance §8
    // fix — silent acceptance is forbidden by the substrate; each
    // consumer signs frames and the receiver verifies against its
    // own enrolled registry. Real embedders typically wire this from
    // an out-of-band trust source (invite, paired QR, signed config).
    let alice_spec: PeerSpec = alice.peer_spec().parse().unwrap();
    let bob_spec: PeerSpec = bob.peer_spec().parse().unwrap();
    alice.add_peer(bob_spec).await.unwrap();
    bob.add_peer(alice_spec).await.unwrap();

    let room = "embedded-smoke";
    alice.join_with_wire(room, wire_path.clone()).await.unwrap();
    bob.join_with_wire(room, wire_path.clone()).await.unwrap();

    // Bob installs a live subscription BEFORE Alice sends so the
    // "no prompt-time polling" path is exercised. The Codex hook
    // case the audit calls out: a consumer subscribes to typed
    // events and receives them as they happen, instead of shelling
    // out to a log-scraping command between turns.
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

    // Store replay. By the time the live event arrived, the wire-side
    // tail must have committed the event to Bob's store too.
    let recent = wait_for_body_text(&bob, first, Duration::from_secs(3)).await;
    assert_eq!(recent.event_id, live.event_id);

    // Cursor-based catch-up. A real embedder restarts on its own
    // schedule and uses the cursor to resume without re-receiving
    // events it already processed. Verify that with a second message:
    // the cursor we capture here MUST exclude `first` from the
    // resume_from page, and INCLUDE the next message Alice sends.
    let cursor_after_first = TranscriptCursor {
        lamport: live.lamport,
        event_id: live.event_id,
    };

    let second = "second message — proves resume_from excludes prior cursor";
    alice.say(second).await.unwrap();

    // Poll resume_from until the new event shows up. A vacuum read
    // against just-after-first must yield `second` and NOT `first`.
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
