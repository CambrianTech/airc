//! Reliability: a live daemon-attached subscription survives a daemon
//! restart — it reconnects and RESUMES strictly after its last cursor,
//! so durable events published while it was reconnecting still arrive.
//!
//! This is the gap `daemon_lifecycle` (CLI) left open: one-shot commands
//! respawn the daemon fine, but a long-lived stream (monitor, codex
//! hook, Continuum) used to go permanently deaf on a daemon bounce.

mod common;

use std::time::Duration;

use airc_core::Body;
use airc_lib::EventStream;
use common::Machine;
use futures::stream::StreamExt;

async fn wait_for_text(stream: &mut EventStream, want: &str, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if event.body.as_ref().and_then(Body::as_text) == Some(want) {
                    return true;
                }
            }
            Ok(Some(Err(_))) => {} // lag marker — keep reading
            Ok(None) => return false,
            Err(_) => {} // poll timeout — keep waiting until deadline
        }
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_subscription_survives_daemon_restart_and_resumes_durable_gap() {
    let mut machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("reconnect-room").await;

    // Bob holds a live subscription across the whole test.
    let mut bob_stream = bob.subscribe().await.expect("bob subscribes");

    // Baseline: delivery works before any restart.
    alice.say("before-restart").await.expect("alice says");
    assert!(
        wait_for_text(&mut bob_stream, "before-restart", Duration::from_secs(3)).await,
        "baseline live delivery must work before the restart"
    );

    // Hard-bounce the daemon (kill + respawn on the same socket + db).
    machine.restart_daemon().await;

    // A durable send after the restart: alice's per-request client
    // reconnects to the new daemon, and bob's *live* stream must
    // reconnect + resume and still deliver it.
    alice
        .say("after-restart")
        .await
        .expect("alice says after restart");
    assert!(
        wait_for_text(&mut bob_stream, "after-restart", Duration::from_secs(10)).await,
        "the live subscription must reconnect after a daemon restart and deliver durable events"
    );
}
