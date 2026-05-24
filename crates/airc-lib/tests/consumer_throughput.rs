//! Continuum-shaped consumer throughput proof.
//!
//! This is not the final Tailnet/LAN target benchmark. It is the
//! CI-safe substrate proof: two independent consumers, separate homes,
//! explicit trust, one shared local wire, live subscription armed
//! before send, and a sustained room event stream at realtime-ish
//! cadence. The real-machine proof can tighten latency thresholds
//! after this shape is stable.

use std::time::{Duration, Instant};

use airc_lib::{Airc, Body, Headers, PeerSpec};
use futures::StreamExt;
use tempfile::TempDir;

const EVENT_COUNT: usize = 90;
const EVENT_HZ: u64 = 60;
const RECEIVE_DRAIN_TIMEOUT: Duration = Duration::from_secs(8);
const CI_P99_MAX: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct PoseEvent {
    seq: usize,
    sent_at: Instant,
}

impl PoseEvent {
    fn encode(&self) -> String {
        // `Instant` is intentionally not serialized. This is a local
        // integration proof, so the receiver gets the sent_at table
        // out-of-band from the harness.
        format!("continuum.pose.fixture seq={}", self.seq)
    }
}

#[tokio::test]
async fn continuum_shaped_pose_stream_delivers_at_60hz_without_drop() {
    let alice_home = TempDir::new().unwrap();
    let bob_home = TempDir::new().unwrap();
    let wire_dir = TempDir::new().unwrap();
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.unwrap();
    let bob = Airc::open(bob_home.path()).await.unwrap();

    let alice_spec: PeerSpec = alice.peer_spec().parse().unwrap();
    let bob_spec: PeerSpec = bob.peer_spec().parse().unwrap();
    alice.add_peer(bob_spec).await.unwrap();
    bob.add_peer(alice_spec).await.unwrap();

    alice
        .join_with_wire("continuum-pose-fixture", wire_path.clone())
        .await
        .unwrap();
    bob.join_with_wire("continuum-pose-fixture", wire_path)
        .await
        .unwrap();

    let mut stream = bob.subscribe().await.unwrap();
    let receiver = tokio::spawn(async move {
        let mut received = Vec::with_capacity(EVENT_COUNT);
        while received.len() < EVENT_COUNT {
            let Some(next) = stream.next().await else {
                break;
            };
            let event = next.expect("live stream event must decode");
            let Some(text) = event.body.as_ref().and_then(Body::as_text) else {
                continue;
            };
            let Some(seq) = text.strip_prefix("continuum.pose.fixture seq=") else {
                continue;
            };
            let seq: usize = seq.parse().expect("fixture seq must be numeric");
            received.push((seq, Instant::now()));
        }
        received
    });

    let interval = Duration::from_nanos(1_000_000_000 / EVENT_HZ);
    let mut sent = Vec::with_capacity(EVENT_COUNT);
    for seq in 0..EVENT_COUNT {
        let pose = PoseEvent {
            seq,
            sent_at: Instant::now(),
        };
        let mut headers = Headers::new();
        headers.insert(
            "forge.contract".to_string(),
            "continuum.pose.fixture".to_string(),
        );
        headers.insert("continuum.fixture.seq".to_string(), seq.to_string());
        alice
            .say_with_headers(&pose.encode(), headers)
            .await
            .unwrap();
        sent.push(pose);
        tokio::time::sleep(interval).await;
    }

    let received = tokio::time::timeout(RECEIVE_DRAIN_TIMEOUT, receiver)
        .await
        .expect("receiver must drain stream after sender finishes")
        .expect("receiver task must join");
    assert_eq!(
        received.len(),
        EVENT_COUNT,
        "pose fixture stream dropped events: received {}/{}",
        received.len(),
        EVENT_COUNT
    );

    let mut seen = vec![false; EVENT_COUNT];
    let mut latencies = Vec::with_capacity(EVENT_COUNT);
    for (seq, received_at) in received {
        assert!(seq < EVENT_COUNT, "received out-of-range seq {seq}");
        assert!(!seen[seq], "duplicate pose event seq {seq}");
        seen[seq] = true;
        latencies.push(received_at.duration_since(sent[seq].sent_at));
    }
    assert!(seen.into_iter().all(|hit| hit), "missing pose event seq");

    latencies.sort_unstable();
    let p50 = percentile(&latencies, 50);
    let p99 = percentile(&latencies, 99);
    assert!(
        p99 <= CI_P99_MAX,
        "CI-safe p99 exceeded: p50={p50:?} p99={p99:?} max={CI_P99_MAX:?}"
    );
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    assert!(!sorted.is_empty(), "percentile requires samples");
    let rank = ((sorted.len() - 1) * percentile) / 100;
    sorted[rank]
}
