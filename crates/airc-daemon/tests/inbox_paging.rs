//! Card 8428ae8c — `Inbox { since: None, limit: N }` against the LIVE
//! daemon: "most recent N" must cost O(N), not O(room).
//!
//! Before this card, the no-cursor inbox path called
//! `resume_from_cursor(channel, None)` — materializing EVERY durable
//! envelope in the room — and then threw away all but the newest N.
//! PR #1144 (card a1562dbc) removed the dominant caller (the tip probe)
//! but any remaining most-recent-N caller still paid the full-room
//! replay. These tests pin the correctness of the paged path (exact
//! tail, ordering, N > room, empty room) and print measured numbers on
//! a deep room so the O(room)→O(N) claim is backed by real data (the
//! structural zero-scan proof lives in `airc-bus/tests/durable_tail.rs`).
//!
//! The test model IS the production model: a real `DaemonState` over a
//! real SQLite ORM on a Unix socket, driven by the real `DaemonClient`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use airc_core::{Headers, PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::{
    DaemonClient, InboxRequest, IpcDelivery, IpcKind, IpcTarget, PublishRequest, PublishResponse,
};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use tokio::task::JoinHandle;

/// A live daemon on a Unix socket, owning a real router + SQLite ORM.
struct TestDaemon {
    socket: PathBuf,
    handle: JoinHandle<()>,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    // Short /tmp path keeps us well under macOS SUN_LEN (104 bytes).
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-ipg-{}-{n}.sock", std::process::id()))
}

async fn start_daemon() -> TestDaemon {
    let home = tempfile::TempDir::new().expect("tempdir");
    let db_path = home.path().join("events.sqlite");
    let peer_id = PeerId::new();
    let keypair = PeerKeypair::generate();
    let registry = PeerKeyRegistry::new();
    registry
        .enrol(peer_id, 0, keypair.public_bytes())
        .expect("enrol self");
    let coordinator: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let state = Arc::new(
        DaemonState::build(
            peer_id,
            keypair,
            Arc::new(registry),
            VerificationPolicy::Strict,
            home.path().to_path_buf(),
            &db_path,
            coordinator,
            DaemonRuntimeInfo::unknown(),
        )
        .await
        .expect("build daemon state"),
    );
    let socket = unique_socket();
    let server_state = state.clone();
    let server_socket = socket.clone();
    let handle = tokio::spawn(async move {
        let _ = run(server_state, server_socket).await;
    });
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    TestDaemon {
        socket,
        handle,
        _home: home,
    }
}

impl TestDaemon {
    async fn stop(self) {
        let _ = DaemonClient::new(self.socket.clone()).stop().await;
        let _ = tokio::time::timeout(Duration::from_secs(3), self.handle).await;
    }
}

fn durable_text(channel: RoomId, text: &str) -> PublishRequest {
    PublishRequest {
        channel: channel.as_uuid(),
        from_peer: uuid::Uuid::from_u128(0xA11CE),
        from_client: uuid::Uuid::from_u128(0x7AB),
        kind: IpcKind::Message,
        delivery: IpcDelivery::Durable,
        target: IpcTarget::All,
        correlation_id: None,
        coalesce_key: None,
        payload: text.as_bytes().to_vec(),
        headers: Headers::new(),
    }
}

fn stream_chunk(channel: RoomId, bytes: &[u8]) -> PublishRequest {
    PublishRequest {
        channel: channel.as_uuid(),
        from_peer: uuid::Uuid::from_u128(0xA11CE),
        from_client: uuid::Uuid::from_u128(0x7AB),
        kind: IpcKind::StreamChunk,
        delivery: IpcDelivery::StreamChunk,
        target: IpcTarget::All,
        correlation_id: None,
        coalesce_key: None,
        payload: bytes.to_vec(),
        headers: Headers::new(),
    }
}

async fn publish_n(client: &DaemonClient, channel: RoomId, n: usize) -> PublishResponse {
    let mut last = None;
    for i in 0..n {
        let request = durable_text(channel, &format!("event {i}"));
        // The write-behind queue is bounded: on a slow runner a tight
        // publish loop can outpace the SQLite drain, and the daemon
        // sheds with a LOUD typed error (§3.8 — the designed
        // back-pressure signal, never a silent drop). Honour the
        // contract the way a real producer would: back off and retry
        // the same event. Any other error is a real failure.
        let mut receipt = None;
        for _ in 0..500 {
            match client.publish(request.clone()).await {
                Ok(r) => {
                    receipt = Some(r);
                    break;
                }
                Err(error) => {
                    let message = error.to_string();
                    assert!(
                        message.contains("saturated"),
                        "unexpected publish error: {message}"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
        last = Some(receipt.expect("publish kept saturating after 500 backoff retries"));
    }
    last.expect("n > 0")
}

/// Decode the payload text out of a wire envelope buffer.
fn payload_text(bytes: Vec<u8>) -> String {
    let env = airc_wire::decode(bytes.into()).expect("decode wire envelope");
    String::from_utf8(env.payload.to_vec()).expect("utf8 payload")
}

/// Correctness pins for the most-recent-N path: exact newest N in
/// ascending order, N > room size returns the whole room, the empty
/// room returns nothing, and a non-durable burst never rides along.
#[tokio::test]
async fn inbox_most_recent_n_returns_exact_ascending_tail() {
    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());
    let channel = RoomId::from_u128(0x7a11);

    // Empty room: no envelopes, no newest cursor.
    let empty = client
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(10),
        })
        .await
        .expect("inbox on empty room");
    assert!(empty.envelopes.is_empty(), "empty room pages empty");
    assert_eq!(empty.newest, None);

    let receipt = publish_n(&client, channel, 25).await;
    // A trailing non-durable burst must not appear in the transcript
    // tail (inbox is the DURABLE transcript).
    for _ in 0..8 {
        client
            .publish(stream_chunk(channel, b"\x01\x02\x03"))
            .await
            .expect("publish chunk");
    }

    // Most recent 10 of 25: events 15..=24, ascending.
    let page = client
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(10),
        })
        .await
        .expect("inbox limit 10");
    let texts: Vec<String> = page.envelopes.into_iter().map(payload_text).collect();
    let expected: Vec<String> = (15..25).map(|i| format!("event {i}")).collect();
    assert_eq!(texts, expected, "exact newest N, ascending order");
    let newest = page.newest.expect("newest cursor");
    assert_eq!(
        (newest.epoch, newest.counter, newest.event_id),
        (receipt.epoch, receipt.counter, receipt.event_id),
        "newest cursor is the last durable publish receipt"
    );

    // N larger than the room: the whole room, still ascending.
    let all = client
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(500),
        })
        .await
        .expect("inbox limit 500");
    let texts: Vec<String> = all.envelopes.into_iter().map(payload_text).collect();
    let expected: Vec<String> = (0..25).map(|i| format!("event {i}")).collect();
    assert_eq!(texts, expected, "N > room size returns the full room");

    daemon.stop().await;
}

/// The honest perf evidence (card 8428ae8c): on a room thousands of
/// events deep, `Inbox { since: None, limit: N }` for small N must cost
/// a fraction of materializing the room. The measured µs/op for the
/// paged probe is printed for the PR body; the hard structural gate
/// (zero full-scan calls) lives in `airc-bus/tests/durable_tail.rs`.
#[tokio::test]
async fn deep_room_most_recent_n_costs_by_n_not_room_depth() {
    const DEEP: usize = 5_000;
    const N: usize = 50;
    const PROBES: u32 = 20;

    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());
    let channel = RoomId::from_u128(0xd44b);

    let receipt = publish_n(&client, channel, DEEP).await;

    // Paged path: most recent N on the deep room.
    let paged_start = Instant::now();
    let mut paged_newest = None;
    let mut paged_len = 0;
    for _ in 0..PROBES {
        let page = client
            .inbox(InboxRequest {
                since: None,
                channel: Some(channel),
                limit: Some(N),
            })
            .await
            .expect("inbox limit N");
        paged_len = page.envelopes.len();
        paged_newest = page.newest;
    }
    let paged_elapsed = paged_start.elapsed();

    // Full-tail shape for comparison: N >= room size genuinely returns
    // every envelope, so its cost is the O(room) floor.
    let full_start = Instant::now();
    let mut full_newest = None;
    let mut full_len = 0;
    for _ in 0..PROBES {
        let page = client
            .inbox(InboxRequest {
                since: None,
                channel: Some(channel),
                limit: Some(DEEP),
            })
            .await
            .expect("inbox limit DEEP");
        full_len = page.envelopes.len();
        full_newest = page.newest;
    }
    let full_elapsed = full_start.elapsed();

    // Same answer at the tail…
    assert_eq!(paged_len, N, "paged path returns exactly N");
    assert_eq!(full_len, DEEP, "full path returns the whole room");
    let paged_newest = paged_newest.expect("paged newest");
    let full_newest = full_newest.expect("full newest");
    assert_eq!(
        paged_newest, full_newest,
        "paged and full tails agree on the newest cursor"
    );
    assert_eq!(
        (paged_newest.epoch, paged_newest.counter),
        (receipt.epoch, receipt.counter),
        "and both are the last publish receipt"
    );

    let paged_us = paged_elapsed.as_micros() / u128::from(PROBES);
    let full_us = full_elapsed.as_micros() / u128::from(PROBES);
    eprintln!(
        "card 8428ae8c: room depth {DEEP}, {PROBES} probes each — \
         inbox(since:None, limit:{N}): {paged_us} µs/op; \
         inbox(since:None, limit:{DEEP}) full tail: {full_us} µs/op"
    );

    // …at strictly lower cost: the paged probe's work tracks N, not the
    // room depth. The margin is deliberately 10x, not 2x: the OLD
    // full-materialize-then-truncate shape already sat ~6x under the
    // full tail (it skipped the encode of 4,950 envelopes), so a 2x
    // assertion could not detect a regression to it. The reverse-paged
    // path measures ~100x under the full tail locally, leaving an order
    // of magnitude of headroom for CI scheduling noise — and the ratio
    // is contention-stable because both legs ride the same daemon.
    assert!(
        paged_elapsed * 10 < full_elapsed,
        "most-recent-{N} ({paged_us} µs/op) must be at least 10x cheaper than \
         materializing the {DEEP}-deep room ({full_us} µs/op) — a fall-back to \
         full-room replay sits ~6x under it and must fail here"
    );

    daemon.stop().await;
}
