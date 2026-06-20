//! Manual latency bench for the LAN request/reply path.
//!
//! Sequential small request→reply round-trips over a real LAN TCP+TLS
//! connection — the shape that exposes Nagle's algorithm + delayed-ACK
//! interaction most clearly (each round-trip waits on the prior, small frames).
//! Used to measure the TCP_NODELAY change on the LAN adapter.
//!
//! `#[ignore]` — it's a measurement (prints a latency table), not a CI assertion.
//! Run: `cargo test -p airc-lib --test lan_latency_bench -- --ignored --nocapture`

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use airc_core::{Body, Headers, MentionTarget, PeerId};
use airc_lib::Airc;
use airc_protocol::{
    HEADER_AIRC_COMMAND_KIND, HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_REPLY_TO,
    HEADER_FORGE_BODY_HINT,
};
use futures::stream::StreamExt;
use tempfile::TempDir;
use uuid::Uuid;

/// Returns (send_leg_us, wait_leg_us): time for `request()` (encode→sign→flush
/// to the wire) vs `await_reply()` (peer processes + replies + delivery back).
/// Splitting the round-trip localizes which half owns the latency.
async fn one_round_trip(alice: &Airc) -> (u64, u64) {
    let mut headers = Headers::new();
    headers.insert(HEADER_AIRC_COMMAND_KIND.into(), "lat.ping".into());
    let t0 = Instant::now();
    let pending = alice
        .request(
            MentionTarget::All,
            headers,
            Body::text("ping"),
            Duration::from_secs(3),
        )
        .await
        .expect("request");
    let send_us = t0.elapsed().as_micros() as u64;
    let t1 = Instant::now();
    alice.await_reply(pending).await.expect("reply");
    let wait_us = t1.elapsed().as_micros() as u64;
    (send_us, wait_us)
}

#[test]
#[ignore = "manual latency measurement; run with --ignored --nocapture"]
fn lan_request_reply_latency() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let alice_home = TempDir::new().unwrap();
        let bob_home = TempDir::new().unwrap();
        let alice = Airc::open(alice_home.path()).await.unwrap();
        let bob = Airc::open(bob_home.path()).await.unwrap();
        let alice_spec = alice.peer_spec().parse().unwrap();
        let bob_spec = bob.peer_spec().parse().unwrap();
        alice.add_peer(bob_spec).await.unwrap();
        bob.add_peer(alice_spec).await.unwrap();
        alice.join("lat").await.unwrap();
        bob.join("lat").await.unwrap();
        let bob_addr = bob
            .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        alice.connect_lan(bob_addr, bob.peer_id()).await.unwrap();

        // Persistent echo responder.
        let bob_handle = bob.clone();
        tokio::spawn(async move {
            let mut stream = bob_handle.subscribe().await.unwrap();
            while let Some(ev) = stream.next().await {
                let Ok(event) = ev else { continue };
                if event.peer_id == bob_handle.peer_id() {
                    continue;
                }
                let (Some(corr), Some(rt)) = (
                    event.headers.get(HEADER_AIRC_CORRELATION_ID),
                    event.headers.get(HEADER_AIRC_REPLY_TO),
                ) else {
                    continue;
                };
                let correlation_id = Uuid::parse_str(corr).unwrap();
                let reply_to = PeerId::from_uuid(Uuid::parse_str(rt).unwrap());
                let mut h = Headers::new();
                h.insert(HEADER_FORGE_BODY_HINT.into(), "lat.pong".into());
                let _ = bob_handle
                    .reply(reply_to, correlation_id, h, Body::text("pong"))
                    .await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        const N: usize = 200;
        for _ in 0..5 {
            one_round_trip(&alice).await; // warmup
        }
        // Turn on seam timing AFTER warmup so the breakdown reflects steady state.
        airc_diagnostics::timing::reset();
        airc_diagnostics::timing::enable();
        let mut total = Vec::with_capacity(N);
        let mut send = Vec::with_capacity(N);
        let mut wait = Vec::with_capacity(N);
        for _ in 0..N {
            let t0 = Instant::now();
            let (s, w) = one_round_trip(&alice).await;
            total.push(t0.elapsed().as_micros() as u64);
            send.push(s);
            wait.push(w);
        }
        let report = |label: &str, v: &mut Vec<u64>| {
            v.sort_unstable();
            let n = v.len();
            println!(
                "  {label:<6} avg={:>6}us  p50={:>6}us  p99={:>6}us  max={:>6}us",
                v.iter().sum::<u64>() / n as u64,
                v[n / 2],
                v[n * 99 / 100],
                v[n - 1],
            );
        };
        airc_diagnostics::timing::disable();
        println!("\n  LAN req/reply latency, {N} sequential round-trips:");
        report("total", &mut total);
        report("send", &mut send); // request(): encode→sign→flush to wire
        report("wait", &mut wait); // await_reply(): peer process + reply delivery

        // Per-seam internal breakdown (airc_diagnostics::timing). Counts span
        // BOTH legs (request send + reply send both go through the transport).
        println!("\n  internal seam breakdown (avg over all sends in the window):");
        for (seam, stat) in airc_diagnostics::timing::snapshot() {
            println!(
                "  {seam:<16} count={:>4}  avg={:>6}us  max={:>6}us",
                stat.count,
                stat.avg_us(),
                stat.max_ns / 1_000,
            );
        }
    });
}
