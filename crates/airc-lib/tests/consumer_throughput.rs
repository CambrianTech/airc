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

const SINGLE_SUBSCRIBER_EVENT_COUNT: usize = 90;
const SINGLE_SUBSCRIBER_EVENT_HZ: u64 = 60;
const FANOUT_EVENT_COUNT: usize = 135;
const FANOUT_EVENT_HZ: u64 = 90;
const FANOUT_SUBSCRIBERS: usize = 3;
const RECEIVE_DRAIN_TIMEOUT: Duration = Duration::from_secs(8);
const CI_P99_MAX: Duration = Duration::from_millis(500);
const CI_FANOUT_P99_MAX: Duration = Duration::from_millis(750);

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
    let result = run_pose_stream_fixture(StreamProfile {
        event_count: SINGLE_SUBSCRIBER_EVENT_COUNT,
        event_hz: SINGLE_SUBSCRIBER_EVENT_HZ,
        subscriber_count: 1,
    })
    .await;

    assert_eq!(
        result.received_per_subscriber,
        vec![SINGLE_SUBSCRIBER_EVENT_COUNT],
        "pose fixture stream dropped events: received {:?}",
        result.received_per_subscriber
    );
    assert!(
        result.p99 <= CI_P99_MAX,
        "CI-safe p99 exceeded: p50={p50:?} p99={p99:?} max={CI_P99_MAX:?}",
        p50 = result.p50,
        p99 = result.p99,
    );
}

#[tokio::test]
async fn continuum_shaped_pose_stream_fans_out_to_three_subscribers_at_90hz() {
    let result = run_pose_stream_fixture(StreamProfile {
        event_count: FANOUT_EVENT_COUNT,
        event_hz: FANOUT_EVENT_HZ,
        subscriber_count: FANOUT_SUBSCRIBERS,
    })
    .await;

    assert_eq!(
        result.received_per_subscriber,
        vec![FANOUT_EVENT_COUNT; FANOUT_SUBSCRIBERS],
        "pose fan-out dropped events: received {:?}",
        result.received_per_subscriber
    );
    assert!(
        result.p99 <= CI_FANOUT_P99_MAX,
        "CI-safe fan-out p99 exceeded: p50={p50:?} p99={p99:?} max={CI_FANOUT_P99_MAX:?}",
        p50 = result.p50,
        p99 = result.p99,
    );
}

#[derive(Debug, Clone, Copy)]
struct StreamProfile {
    event_count: usize,
    event_hz: u64,
    subscriber_count: usize,
}

#[derive(Debug)]
struct StreamResult {
    received_per_subscriber: Vec<usize>,
    p50: Duration,
    p99: Duration,
}

async fn run_pose_stream_fixture(profile: StreamProfile) -> StreamResult {
    let alice_home = TempDir::new().unwrap();
    let subscriber_homes: Vec<_> = (0..profile.subscriber_count)
        .map(|_| TempDir::new().unwrap())
        .collect();
    let wire_dir = TempDir::new().unwrap();
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.unwrap();
    let mut subscribers = Vec::with_capacity(profile.subscriber_count);
    for home in &subscriber_homes {
        subscribers.push(Airc::open(home.path()).await.unwrap());
    }

    let alice_spec: PeerSpec = alice.peer_spec().parse().unwrap();
    for subscriber in &subscribers {
        let subscriber_spec: PeerSpec = subscriber.peer_spec().parse().unwrap();
        alice.add_peer(subscriber_spec).await.unwrap();
        subscriber.add_peer(alice_spec.clone()).await.unwrap();
    }

    alice
        .join_with_wire("continuum-pose-fixture", wire_path.clone())
        .await
        .unwrap();
    for subscriber in &subscribers {
        subscriber
            .join_with_wire("continuum-pose-fixture", wire_path.clone())
            .await
            .unwrap();
    }

    let mut receivers = Vec::with_capacity(profile.subscriber_count);
    for subscriber in &subscribers {
        let stream = subscriber.subscribe().await.unwrap();
        receivers.push(tokio::spawn(receive_pose_stream(
            stream,
            profile.event_count,
        )));
    }

    let interval = Duration::from_nanos(1_000_000_000 / profile.event_hz);
    let mut sent = Vec::with_capacity(profile.event_count);
    for seq in 0..profile.event_count {
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

    let mut received_per_subscriber = Vec::with_capacity(profile.subscriber_count);
    let mut latencies = Vec::with_capacity(profile.event_count * profile.subscriber_count);
    for receiver in receivers {
        let received = tokio::time::timeout(RECEIVE_DRAIN_TIMEOUT, receiver)
            .await
            .expect("receiver must drain stream after sender finishes")
            .expect("receiver task must join");
        received_per_subscriber.push(received.len());

        let mut seen = vec![false; profile.event_count];
        for (seq, received_at) in received {
            assert!(seq < profile.event_count, "received out-of-range seq {seq}");
            assert!(!seen[seq], "duplicate pose event seq {seq}");
            seen[seq] = true;
            latencies.push(received_at.duration_since(sent[seq].sent_at));
        }
        assert!(seen.into_iter().all(|hit| hit), "missing pose event seq");
    }

    latencies.sort_unstable();
    StreamResult {
        received_per_subscriber,
        p50: percentile(&latencies, 50),
        p99: percentile(&latencies, 99),
    }
}

async fn receive_pose_stream(
    mut stream: airc_lib::EventStream,
    event_count: usize,
) -> Vec<(usize, Instant)> {
    let mut received = Vec::with_capacity(event_count);
    while received.len() < event_count {
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
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    assert!(!sorted.is_empty(), "percentile requires samples");
    let rank = ((sorted.len() - 1) * percentile) / 100;
    sorted[rank]
}
