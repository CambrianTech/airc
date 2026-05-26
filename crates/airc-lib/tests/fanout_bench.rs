//! Fan-out delivery-latency benchmark for the airc same-machine
//! substrate. Produces hard p50/p99 numbers comparing the two
//! same-machine delivery paths at continuum-persona scale, so the
//! performant bus (PR 2) can be designed from real measurements.
//!
//! Motivating concern: ~14 continuum personas + a UI send/receive at
//! once. The current same-machine transport is a flat `frames.jsonl`
//! polled every 50ms per subscriber
//! (`crates/airc-transport/src/local_fs.rs`).
//!
//! TWO PATHS, measured the same way (send Instant -> receive Instant,
//! carrying the seq->Instant table out-of-band since `Instant` is not
//! serialized):
//!
//! - Path A — POLLED WIRE (current same-machine bus): K subscriber
//!   `Airc` instances on SEPARATE homes sharing ONE wire_root,
//!   cross-trusted, each arming a live subscription; one separate
//!   publisher `Airc` instance sends N events. Cross-instance delivery
//!   here MUST traverse the polled local-fs wire subscriber, because
//!   `live_tx` is per-instance — the publisher's `live_tx` is a
//!   different broadcast channel from each subscriber's. The only way
//!   the bytes reach a subscriber's `live_tx` is via that subscriber's
//!   own wire tail loop:
//!     local_fs.rs `sleep(POLL_INTERVAL)` (50ms) -> tail loop yields
//!     frame -> transport.rs `append_received_frame(frame)` ->
//!     transport.rs `live_tx.send(...)`.
//!
//! - Path B — IN-PROCESS BROADCAST (what embedding airc-lib gives
//!   continuum): ONE `Airc` instance with K `subscribe()` streams
//!   armed (models 14 persona subscriptions inside one embedded
//!   runtime); the SAME instance publishes N events; delivery goes
//!   through the in-process `live_tx` broadcast synchronously inside
//!   `append_sent_frame` (messaging.rs `live_tx.send(...)`), no file,
//!   no poll.
//!
//! Run:
//!   cargo test --release -p airc-lib --test fanout_bench -- --ignored --nocapture

use std::time::{Duration, Instant};

use airc_lib::{Airc, Body, Headers, MentionTarget, PeerSpec};
use airc_protocol::FrameKind;
use futures::StreamExt;
use tempfile::TempDir;

/// Events per run. ~200 per the brief.
const EVENT_COUNT: usize = 200;
/// Subscriber counts to sweep: 1, 4, 14 (14 ~= continuum persona scale).
const SUBSCRIBER_COUNTS: [usize; 3] = [1, 4, 14];
/// Modest fixed cadence between sends.
const SEND_INTERVAL: Duration = Duration::from_millis(1);
/// Generous drain timeout — Path A is poll-bound and a slow CI box can
/// stack many 50ms poll cycles, so give the receivers plenty of room.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

const CONTRACT_HEADER: &str = "forge.contract";
const CONTRACT_VALUE: &str = "fanout.bench.fixture";
const SEQ_PREFIX: &str = "fanout.bench.fixture seq=";

#[derive(Debug)]
struct Sample {
    p50: Duration,
    p99: Duration,
    events_per_sec: f64,
    received_per_subscriber: Vec<usize>,
}

// Multi-threaded runtime is REQUIRED for an honest measurement. On the
// default current-thread runtime, the publisher's send loop (each `say`
// brackets an fsync in `spawn_blocking`) and the K receiver tasks all
// contend for one executor thread, so a receiver can't be scheduled to
// drain its broadcast channel until the executor yields — that
// scheduling delay (not the delivery path) dominates, and Path A vs
// Path B collapse to the same number. With real worker threads the
// in-process broadcast (Path B) shows its true sub-ms delivery and the
// poll-bound wire (Path A) shows its ~50ms floor.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark — run explicitly with --ignored --nocapture in release"]
async fn fanout_latency_polled_wire_vs_in_process_broadcast() {
    println!();
    println!("=== airc fan-out delivery-latency benchmark ===");
    println!(
        "events/run={EVENT_COUNT}  send_interval={:?}  drain_timeout={:?}",
        SEND_INTERVAL, DRAIN_TIMEOUT
    );
    println!();
    println!(
        "{:<28} {:>3} {:>12} {:>12} {:>14}  {}",
        "path", "K", "p50", "p99", "events/sec", "delivered"
    );
    println!("{}", "-".repeat(92));

    let mut rows: Vec<(&'static str, usize, Sample)> = Vec::new();

    for &k in &SUBSCRIBER_COUNTS {
        let sample = run_path_a_polled_wire(k).await;
        print_row("A polled-wire (local-fs)", k, &sample);
        rows.push(("A", k, sample));
    }
    for &k in &SUBSCRIBER_COUNTS {
        let sample = run_path_b_in_process(k, FrameKind::Event).await;
        print_row("B in-process (live_tx)", k, &sample);
        rows.push(("B", k, sample));
    }
    // Durable (Message-kind) over the same in-process broadcast. This
    // is the slice-1 proof: deliver-first reorder moved the ~27ms wire
    // fsync AFTER the live_tx fan-out, so Durable receive latency should
    // now be low-ms (broadcast time), not ~27ms. Pre-reorder this row
    // would read ~27ms.
    for &k in &SUBSCRIBER_COUNTS {
        let sample = run_path_b_in_process(k, FrameKind::Message).await;
        print_row("B-durable (live_tx, Message)", k, &sample);
        rows.push(("B-durable", k, sample));
    }

    println!("{}", "-".repeat(92));
    println!();

    // Methodology sanity checks. These are reported, not hard-asserted,
    // so the table always prints; but a violation means the measurement
    // is wrong and is flagged loudly.
    run_sanity_checks(&rows);

    // Completeness: report Event-frame delivery loss per path. Event
    // frames are interrupt-style and lossy on a saturated per-subscriber
    // buffer (local_fs.rs:400-414 for Path A; broadcast lag for Path B),
    // so this is a reported finding for the bus design — NOT a hard
    // assertion. At this modest 1ms cadence we expect zero loss on both.
    println!("=== delivery completeness (Event-kind, lossy by contract) ===");
    let mut any_loss = false;
    for (path, k, sample) in &rows {
        let dropped: usize = sample
            .received_per_subscriber
            .iter()
            .map(|&n| EVENT_COUNT.saturating_sub(n))
            .sum();
        any_loss |= dropped > 0;
        println!(
            "Path {path} K={k}: delivered {:?} of {EVENT_COUNT} each ({dropped} dropped total)",
            sample.received_per_subscriber
        );
    }
    println!(
        "completeness: {}",
        if any_loss {
            "SOME EVENT FRAMES DROPPED (expected at higher cadence; see counts above)"
        } else {
            "ZERO LOSS on both paths at this cadence"
        }
    );

    // Guard rail: every (path, K) must deliver SOMETHING, or the harness
    // wiring is broken (e.g. subscription not armed, wrong wire root).
    for (path, k, sample) in &rows {
        assert!(
            sample.received_per_subscriber.iter().all(|&n| n > 0),
            "path {path} K={k} delivered nothing: {:?} — harness misconfigured",
            sample.received_per_subscriber
        );
    }
}

fn print_row(path: &str, k: usize, sample: &Sample) {
    println!(
        "{:<28} {:>3} {:>12} {:>12} {:>14.0}  {:?}",
        path,
        k,
        format!("{:?}", sample.p50),
        format!("{:?}", sample.p99),
        sample.events_per_sec,
        sample.received_per_subscriber,
    );
}

fn run_sanity_checks(rows: &[(&'static str, usize, Sample)]) {
    println!("=== methodology sanity checks ===");
    let mut all_ok = true;

    // Check 1: Path A p99 should be on the order of the 50ms poll
    // interval or higher (it's poll-bound).
    for (_path, k, sample) in rows.iter().filter(|(p, _, _)| *p == "A") {
        let ok = sample.p99 >= Duration::from_millis(25);
        all_ok &= ok;
        println!(
            "[{}] Path A K={k} p99={:?} >= ~poll interval (>=25ms expected, poll=50ms)",
            mark(ok),
            sample.p99
        );
    }

    // Check 2: Path B should be poll-FREE — low-single-digit-ms, well
    // under the 50ms poll floor. The live_tx broadcast itself is
    // sub-microsecond; the residual few ms is send-side: `say`/`send`
    // still route through `execute_send_route` -> LocalFsAdapter::send
    // (ensure_wire_dir stat + flock + open + write, NO fsync for Event
    // kind) BEFORE the live_tx fan-out at messaging.rs:133, plus tokio
    // scheduling across the fan-out. Bar: p99 < 25ms (i.e. strictly
    // below half the poll interval, so it can never be confused with a
    // poll-bound number).
    for (_path, k, sample) in rows.iter().filter(|(p, _, _)| *p == "B") {
        let ok = sample.p99 < Duration::from_millis(25);
        all_ok &= ok;
        println!(
            "[{}] Path B K={k} p50={:?} p99={:?} poll-free low-single-digit-ms (p99<25ms expected)",
            mark(ok),
            sample.p50,
            sample.p99
        );
    }

    // Check 2b (slice-1 deliver-first proof): Durable/Message-kind over
    // the in-process broadcast must ALSO be low-ms. The wire fsync
    // (~27ms) now runs AFTER the live_tx fan-out, so receive latency is
    // the broadcast time, not the fsync. Pre-reorder this read ~27ms.
    for (_path, k, sample) in rows.iter().filter(|(p, _, _)| *p == "B-durable") {
        let ok = sample.p99 < Duration::from_millis(25);
        all_ok &= ok;
        println!(
            "[{}] B-durable K={k} p50={:?} p99={:?} fsync now AFTER fan-out (p99<25ms expected; ~27ms pre-reorder)",
            mark(ok),
            sample.p50,
            sample.p99
        );
    }

    // Check 3: Path A and Path B must NOT come out similar — if they
    // do, the same path was accidentally measured. Compare per-K p50:
    // Path A should be at least ~10x Path B.
    for &k in &SUBSCRIBER_COUNTS {
        let a = rows
            .iter()
            .find(|(p, kk, _)| *p == "A" && *kk == k)
            .map(|(_, _, s)| s.p50);
        let b = rows
            .iter()
            .find(|(p, kk, _)| *p == "B" && *kk == k)
            .map(|(_, _, s)| s.p50);
        if let (Some(a), Some(b)) = (a, b) {
            // Guard against div-by-zero on a sub-microsecond Path B p50.
            let b_floor = b.max(Duration::from_micros(1));
            let ratio = a.as_secs_f64() / b_floor.as_secs_f64();
            let ok = ratio >= 5.0;
            all_ok &= ok;
            println!(
                "[{}] K={k} distinct-path: A.p50 {:?} vs B.p50 {:?} (ratio {:.1}x, >=5x expected)",
                mark(ok),
                a,
                b,
                ratio
            );
        }
    }

    println!();
    println!(
        "sanity summary: {}",
        if all_ok {
            "ALL HELD"
        } else {
            "VIOLATION(S) DETECTED — see [FAIL] rows above"
        }
    );
    println!();
}

fn mark(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}

// ---------------------------------------------------------------------
// Path A: polled wire (cross-instance, separate homes, one wire_root).
// ---------------------------------------------------------------------
async fn run_path_a_polled_wire(subscriber_count: usize) -> Sample {
    let publisher_home = TempDir::new().unwrap();
    let subscriber_homes: Vec<_> = (0..subscriber_count)
        .map(|_| TempDir::new().unwrap())
        .collect();
    let wire_dir = TempDir::new().unwrap();
    let wire_path = wire_dir.path().join("wire.jsonl");

    let publisher = Airc::open(publisher_home.path()).await.unwrap();
    let mut subscribers = Vec::with_capacity(subscriber_count);
    for home in &subscriber_homes {
        subscribers.push(Airc::open(home.path()).await.unwrap());
    }

    // Cross-trust: every subscriber trusts the publisher and vice versa,
    // so frame verification on the wire passes both directions.
    let publisher_spec: PeerSpec = publisher.peer_spec().parse().unwrap();
    for subscriber in &subscribers {
        let subscriber_spec: PeerSpec = subscriber.peer_spec().parse().unwrap();
        publisher.add_peer(subscriber_spec).await.unwrap();
        subscriber.add_peer(publisher_spec.clone()).await.unwrap();
    }

    // One shared wire_root. Publisher + all subscribers join it.
    publisher
        .join_with_wire("fanout-bench", wire_path.clone())
        .await
        .unwrap();
    for subscriber in &subscribers {
        subscriber
            .join_with_wire("fanout-bench", wire_path.clone())
            .await
            .unwrap();
    }

    // Arm a LIVE subscription per subscriber BEFORE sending. Each
    // subscriber's subscribe() also ensures its own wire tail loop is
    // running (the only bridge from the shared file to that
    // subscriber's per-instance live_tx).
    let mut receivers = Vec::with_capacity(subscriber_count);
    for subscriber in &subscribers {
        let stream = subscriber.subscribe().await.unwrap();
        receivers.push(tokio::spawn(receive_stream(stream, EVENT_COUNT)));
    }

    // Give every subscriber's tail loop a couple of poll cycles to seek
    // to EOF before the first send, so no live event is missed at the
    // start of the stream.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let sent = send_stream(&publisher, FrameKind::Event).await;
    let send_started = sent.first().map(|p| p.1).unwrap();

    collect(receivers, sent, send_started).await
}

// ---------------------------------------------------------------------
// Path B: in-process broadcast (one instance, K subscribe() streams).
// ---------------------------------------------------------------------
async fn run_path_b_in_process(subscriber_count: usize, kind: FrameKind) -> Sample {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("fanout-bench").await.unwrap();

    // K live subscriptions on the SAME instance. They all share this
    // instance's single live_tx broadcast — the path embedding
    // continuum gets for its persona subscriptions.
    let mut receivers = Vec::with_capacity(subscriber_count);
    for _ in 0..subscriber_count {
        let stream = airc.subscribe().await.unwrap();
        receivers.push(tokio::spawn(receive_stream(stream, EVENT_COUNT)));
    }

    // The same instance publishes. append_sent_frame fans out to
    // live_tx synchronously with the send — no file, no poll. Clone
    // the handle so sends share the same live_tx the streams subscribed
    // to (a fresh open would allocate a different broadcast channel).
    let publisher = airc.clone();
    let sent = send_stream(&publisher, kind).await;
    let send_started = sent.first().map(|p| p.1).unwrap();

    collect(receivers, sent, send_started).await
}

// ---------------------------------------------------------------------
// Shared send / receive / collect machinery.
// ---------------------------------------------------------------------

/// Send EVENT_COUNT Event-kind frames at SEND_INTERVAL cadence,
/// recording (seq, sent_at Instant) out-of-band. `Instant` is
/// deliberately not serialized — the receiver reports seq, and we look
/// up the matching sent_at here.
///
/// `kind` is parameterized. Path A and the primary Path B row use
/// `FrameKind::Event`, which isolates the structural delivery cost the
/// bus design cares about — 50ms poll (Path A) vs in-process broadcast
/// (Path B) — from the orthogonal macOS `sync_data()` fsync that
/// Message frames pay on every wire write (local_fs.rs
/// `write_then_maybe_sync`, ~27ms on this box). The extra
/// "B-durable" row sends `FrameKind::Message` specifically to measure
/// the deliver-first reorder: `append_sent_frame` (persist + live_tx
/// fan-out) now runs BEFORE `execute_send_route` (the fsync'ing wire
/// write) in `send_frame_to_room`, so Durable receive latency should be
/// low-ms (broadcast time) instead of ~27ms (post-fsync). Event frames
/// skip fsync (local_fs.rs:218-223) and are what continuum's
/// high-frequency persona traffic (pose/typing/turn) rides on.
async fn send_stream(publisher: &Airc, kind: FrameKind) -> Vec<(usize, Instant)> {
    let mut sent = Vec::with_capacity(EVENT_COUNT);
    for seq in 0..EVENT_COUNT {
        let mut headers = Headers::new();
        headers.insert(CONTRACT_HEADER.to_string(), CONTRACT_VALUE.to_string());
        headers.insert("fanout.bench.seq".to_string(), seq.to_string());
        let now = Instant::now();
        publisher
            .send_frame_to_for_test(
                kind,
                MentionTarget::All,
                Body::text(format!("{SEQ_PREFIX}{seq}")),
                headers,
            )
            .await
            .unwrap();
        sent.push((seq, now));
        tokio::time::sleep(SEND_INTERVAL).await;
    }
    sent
}

/// Idle window: once the stream goes quiet this long after the sender
/// has finished, the receiver stops waiting. Must comfortably exceed
/// one 50ms poll cycle so Path A isn't cut off mid-poll, but bounded so
/// a dropped Event frame (Path A's documented lossy semantic on a full
/// per-subscriber buffer) doesn't hang the drain forever.
const RECEIVE_IDLE_WINDOW: Duration = Duration::from_millis(500);

/// Drain a live stream until `event_count` fixture events are seen or
/// the stream goes idle for RECEIVE_IDLE_WINDOW, stamping the receive
/// Instant per event. Event frames are lossy on a saturated
/// per-subscriber buffer (local_fs.rs:400-414), so we cannot block
/// indefinitely on a fixed count.
async fn receive_stream(
    mut stream: airc_lib::EventStream,
    event_count: usize,
) -> Vec<(usize, Instant)> {
    let mut received = Vec::with_capacity(event_count);
    while received.len() < event_count {
        let next = match tokio::time::timeout(RECEIVE_IDLE_WINDOW, stream.next()).await {
            Ok(Some(next)) => next,
            // Stream ended, or quiet past the idle window — done draining.
            Ok(None) | Err(_) => break,
        };
        let event = match next {
            Ok(event) => event,
            // A broadcast lag means events were dropped before this
            // subscriber pulled them. Surface it as a counted gap rather
            // than panicking; the dropped-count shows up in the
            // delivered column.
            Err(_lag) => continue,
        };
        let Some(text) = event.body.as_ref().and_then(Body::as_text) else {
            continue;
        };
        let Some(seq) = text.strip_prefix(SEQ_PREFIX) else {
            continue;
        };
        let seq: usize = seq.parse().expect("fixture seq must be numeric");
        received.push((seq, Instant::now()));
    }
    received
}

/// Join all receivers, compute per-(event, subscriber) latency from the
/// out-of-band sent_at table, and roll up p50/p99/events-per-sec.
async fn collect(
    receivers: Vec<tokio::task::JoinHandle<Vec<(usize, Instant)>>>,
    sent: Vec<(usize, Instant)>,
    send_started: Instant,
) -> Sample {
    let mut sent_at = vec![None; EVENT_COUNT];
    for (seq, instant) in &sent {
        sent_at[*seq] = Some(*instant);
    }

    let mut received_per_subscriber = Vec::with_capacity(receivers.len());
    let mut latencies: Vec<Duration> = Vec::with_capacity(EVENT_COUNT * receivers.len());
    let mut last_receive = send_started;

    for receiver in receivers {
        let received = tokio::time::timeout(DRAIN_TIMEOUT, receiver)
            .await
            .expect("receiver must drain stream after sender finishes")
            .expect("receiver task must join");
        received_per_subscriber.push(received.len());
        for (seq, received_at) in received {
            let sent_at = sent_at[seq].expect("received seq must have a sent_at");
            latencies.push(received_at.duration_since(sent_at));
            if received_at > last_receive {
                last_receive = received_at;
            }
        }
    }

    latencies.sort_unstable();
    let total_delivered = latencies.len() as f64;
    let wall = last_receive.duration_since(send_started).as_secs_f64();
    let events_per_sec = if wall > 0.0 {
        total_delivered / wall
    } else {
        f64::INFINITY
    };

    Sample {
        p50: percentile(&latencies, 50),
        p99: percentile(&latencies, 99),
        events_per_sec,
        received_per_subscriber,
    }
}

fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    assert!(!sorted.is_empty(), "percentile requires samples");
    let rank = ((sorted.len() - 1) * pct) / 100;
    sorted[rank]
}
