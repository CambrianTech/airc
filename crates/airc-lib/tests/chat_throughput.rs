//! Card 127816bd Phase 1.A — chat-message throughput bench.
//!
//! Written FIRST per TDD/VDD discipline (continuation of the pattern
//! from #1077 / #1078 / #1079 / #1083): the bench is the substrate
//! property; the optimization comes after the measurement.
//!
//! The workload shape comes from the continuum agent's
//! [substrate-q] message (peer 9bb24964, 2026-05-29) — the chat
//! layer's realistic production shape is "15 personas × chat
//! message every 3s sustained (= 5 msg/s) + 200-msg burst replay on
//! attach." This bench encodes that shape so any future change to
//! the per-message hot path either holds the floor or trips the
//! assertion.
//!
//! ## What this measures
//!
//! Two paired `Airc` handles on loopback LAN-TCP (alice publishes,
//! bob listens). The substrate refuses to publish without an
//! admissible cross-machine route — `FrameKind::Message` over UDP
//! is explicitly priority-255 for `DataInteractive` (UDP is the
//! media-signaling transport, not the chat transport). LAN-TCP is
//! the production analogue we can reach without standing up a
//! daemon. The bench captures: envelope build + ORM dual-write +
//! live_tx fan-out + loopback TLS send.
//!
//! Daemon IPC has its own bench (#1079). Wire framing has its own
//! bench (#1077). This one isolates the substrate's per-message
//! work — the layer card 127816bd targets.
//!
//! ## Phase 1.A baseline (2026-05-29, M2 paired loopback LAN)
//!
//! - Sustained `say()` (~280-char body, no headers): **~3.56 ms/op,
//!   281 msg/sec** (300 calls in 1.07s)
//! - Burst `say_with_headers()` (~280-char body, 2 headers):
//!   **~3.61 ms/op** (200 calls in 723ms)
//! - Minimal `say("x")` (1-char body, no headers): **~3.71 ms/op**
//!   (500 calls in 1.86s)
//!
//! ## Phase 1.C result (WAL + sync=NORMAL + drop post-insert SELECT)
//!
//! - Sustained: **~2.01 ms/op, 498 msg/sec** (1.77× over baseline)
//! - Burst:     **~1.94 ms/op** (1.86× over baseline)
//! - Minimal:   **~1.87 ms/op** (1.98× over baseline)
//!
//! Goal continuum cited: 100 μs/op. Still ~20× above that; Phase 2+
//! attacks the residual cost (LAN-TCP TLS write + envelope sign).
//!
//! The minimal-headers variant moves with the realistic-payload
//! variant ⇒ per-msg cost is dominated by something **invariant to
//! payload size** — the SQLite fsync + INSERT-SELECT round trip
//! (now both eliminated). Body size cost did not surface as a
//! bottleneck at this scale.
//!
//! ## Acceptance floors
//!
//! Bench is a *regression catcher* at the Phase 1.C floor; 5ms/op
//! ceiling = ~2.5× Phase 1.C measured (passes typical fsync-jitter
//! variance, catches a 2.5× regression). Tighten further when the
//! next phase ships and the new floor is durable.
//!
//! Actual ns/op numbers printed via `eprintln!` so a perf reviewer
//! reads the truth off the test output, not the assertion.
//!
//! ## What ships in subsequent phases
//!
//! Phase 1.B-D in the same card: identify which ORM hits are
//! per-message vs amortizable, collapse to one transcript append,
//! re-bench against this baseline. Phase 1.D's success criterion is
//! "ns/op is decisively lower than this Phase 1.A baseline AND
//! continuum's dual-write shim can be deleted in PR #1442."

use std::net::SocketAddr;

use airc_core::headers::Headers;
use airc_lib::{Airc, PeerSpec};
use tempfile::TempDir;

/// Two paired `Airc` handles on loopback LAN-TCP — alice publishes,
/// bob listens. This is the real production hot path for chat
/// delivery (modulo the daemon-IPC layer, which has its own bench
/// in #1079): substrate publish → envelope/ORM/live_tx → wire write.
///
/// The substrate refuses to deliver `FrameKind::Message` without an
/// admissible cross-machine route (UDP is rejected for
/// `DataInteractive` — priority 255 — because UDP is the
/// media-signaling transport, not the chat transport). LAN-TCP is the
/// closest production analogue available without a daemon.
///
/// Returns `(alice, bob, _bob_home, _alice_home)` — the `TempDir`s
/// are kept alive for the lifetime of the test by binding them in
/// the caller's scope.
async fn paired_airc(alice_home: std::path::PathBuf, bob_home: std::path::PathBuf) -> (Airc, Airc) {
    let alice = Airc::open(alice_home).await.expect("alice open");
    let bob = Airc::open(bob_home).await.expect("bob open");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob spec");
    alice.add_peer(bob_spec).await.expect("alice trusts bob");
    bob.add_peer(alice_spec).await.expect("bob trusts alice");

    alice
        .join("chat-throughput-bench")
        .await
        .expect("alice join");
    bob.join("chat-throughput-bench").await.expect("bob join");

    let bob_addr: SocketAddr = bob
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bob listens");
    alice
        .connect_lan(bob_addr, bob.peer_id())
        .await
        .expect("alice connects to bob");

    (alice, bob)
}

#[tokio::test]
#[ignore = "perf bench: contends for fsync + TCP loopback; opt-in via `--ignored --test-threads=1`"]
async fn bench_chat_throughput_sustained_load() {
    // 15 personas × 5 msg/s sustained = 75 msg/s. Simulate one
    // persona's cost; the substrate's per-message hot path is
    // identity-independent at this layer (envelope construction +
    // ORM append don't depend on which peer is sending — they
    // depend on the message itself). The 15× multiplier matters
    // when we measure contention, not when we measure per-call cost.
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let (airc, _bob) = paired_airc(tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()).await;

    // Warmup — first call pays the channel-resolve + identity-load
    // cost; subsequent calls hit the in-memory cached paths. Same
    // shape as production where the daemon stays warm.
    for i in 0..50 {
        airc.say(&format!("warmup {i}")).await.expect("warmup send");
    }

    // Workload synthesizer matches the continuum agent's spec:
    // 5 msg/s × 60s = 300 messages. At 1ms/msg floor that's 300ms
    // total; the bench finishes in a fraction of a second on M2.
    const N: u64 = 300;
    let start = std::time::Instant::now();
    for i in 0..N {
        airc.say(&format!("sustained message {i}"))
            .await
            .expect("send");
    }
    let elapsed = start.elapsed();
    let ns_per_op = elapsed.as_nanos() as u64 / N;
    let msg_per_sec = 1_000_000_000 / ns_per_op.max(1);
    eprintln!(
        "card 127816bd Phase 1.A: chat sustained — {N} say() calls in {elapsed:?}, \
         {ns_per_op} ns/op, {msg_per_sec} msg/sec"
    );

    // Regression floor at 3× measured baseline (~3.56 ms/op → 10ms
    // ceiling). Catches catastrophic regression; permissive enough to
    // survive CI noise. Phase 1.D tightens to whatever the optimized
    // value lands at.
    assert!(
        ns_per_op < 5_000_000,
        "chat sustained throughput regressed to {ns_per_op} ns/op — \
         was the per-message hot path hit with new ORM round-trips?"
    );
}

#[tokio::test]
#[ignore = "perf bench: contends for fsync + TCP loopback; opt-in via `--ignored --test-threads=1`"]
async fn bench_chat_burst_replay_attach() {
    // The "burst" shape: a chat widget mounts, pulls last 200
    // messages off the store, renders. We measure the cost of
    // EMITTING those 200 messages back-to-back — that's the
    // adversarial case for the per-message hot path (no inter-call
    // breathing room, no async slack).
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let (airc, _bob) = paired_airc(tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()).await;

    for i in 0..20 {
        airc.say(&format!("warmup {i}")).await.expect("warmup");
    }

    const BURST: u64 = 200;
    let mut headers = Headers::new();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.persona.turn.v1".to_string(),
    );
    headers.insert("continuum.widget".to_string(), "video-room".to_string());

    let start = std::time::Instant::now();
    for i in 0..BURST {
        airc.say_with_headers(
            &format!(
                "burst {i}: 280-char body simulating a typical persona turn \
                     so the realistic payload size is in the measurement; the \
                     mid-length prose mirrors what continuum's chat-widget \
                     actually carries when a persona finishes a turn."
            ),
            headers.clone(),
        )
        .await
        .expect("burst send");
    }
    let elapsed = start.elapsed();
    let ns_per_op = elapsed.as_nanos() as u64 / BURST;
    eprintln!(
        "card 127816bd Phase 1.A: chat burst — {BURST} say_with_headers() calls \
         (realistic 280-char body + 2 headers) in {elapsed:?}, \
         {ns_per_op} ns/op"
    );

    // Regression floor at 3× measured baseline (~723ms → 2200ms
    // ceiling) for the burst total. The 200-msg replay shape becomes
    // user-visible long before this; Phase 1.D's optimization is what
    // tightens this floor to "no longer user-visible."
    assert!(
        elapsed.as_millis() < 1100,
        "chat burst replay regressed to {elapsed:?} total ({BURST} messages); \
         continuum's chat-widget mount becomes user-visible at this scale"
    );
}

#[tokio::test]
#[ignore = "perf bench: contends for fsync + TCP loopback; opt-in via `--ignored --test-threads=1`"]
async fn bench_chat_throughput_minimal_headers() {
    // Sanity bench: same shape as `bench_chat_throughput_sustained_load`
    // but with NO headers, NO body customization, so it isolates the
    // pure-substrate overhead from any payload-shape effects. If
    // Phase 1.B-D's optimization claims X% off the realistic load
    // but this bench moves by a different factor, that's diagnostic
    // information (the per-message cost vs the per-byte cost).
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let (airc, _bob) = paired_airc(tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()).await;

    for _ in 0..50 {
        airc.say("warmup").await.expect("warmup");
    }

    const N: u64 = 500;
    let start = std::time::Instant::now();
    for _ in 0..N {
        airc.say("x").await.expect("send");
    }
    let elapsed = start.elapsed();
    let ns_per_op = elapsed.as_nanos() as u64 / N;
    eprintln!(
        "card 127816bd Phase 1.A: chat minimal — {N} say(\"x\") calls in {elapsed:?}, \
         {ns_per_op} ns/op (pure-substrate baseline; no headers, 1-char body)"
    );

    assert!(
        ns_per_op < 5_000_000,
        "minimal say() regressed to {ns_per_op} ns/op — substrate per-msg overhead has grown"
    );
}
