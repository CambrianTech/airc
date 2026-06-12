//! Card a1562dbc — `room_tip` against the LIVE daemon: the O(1) probe
//! for "what is the newest durable cursor on this room?"
//!
//! Before this op, the only way to learn the tip over IPC was
//! `Inbox { since: None, limit: 1 }` — which makes the daemon replay
//! the ENTIRE room (`resume_from_cursor(channel, None)` materializes
//! every durable envelope) and throw away all but the last. These tests
//! prove the typed probe returns exactly the same cursor the scan
//! would, and print the measured cost of both paths on a deep room so
//! the O(n)→O(1) claim is backed by real numbers (the structural
//! zero-scan proof lives in `airc-bus/tests/durable_tip.rs`).
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
    RoomTipRequest,
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
    PathBuf::from(format!("/tmp/airc-rtp-{}-{n}.sock", std::process::id()))
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
        let receipt = client
            .publish(durable_text(channel, &format!("event {i}")))
            .await
            .expect("publish");
        last = Some(receipt);
    }
    last.expect("n > 0")
}

#[tokio::test]
async fn room_tip_is_none_for_empty_room_and_tracks_publishes() {
    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());
    let channel = RoomId::from_u128(0x711);

    // Empty room: no tip.
    let tip = client
        .room_tip(RoomTipRequest { channel })
        .await
        .expect("room_tip");
    assert_eq!(tip.tip, None, "empty room has no tip");

    // One publish: the tip IS the receipt.
    let receipt = publish_n(&client, channel, 1).await;
    let tip = client
        .room_tip(RoomTipRequest { channel })
        .await
        .expect("room_tip")
        .tip
        .expect("tip after publish");
    assert_eq!((tip.epoch, tip.counter), (receipt.epoch, receipt.counter));
    assert_eq!(tip.event_id, receipt.event_id);

    // More publishes: the tip advances to each newest receipt.
    let receipt = publish_n(&client, channel, 5).await;
    let tip = client
        .room_tip(RoomTipRequest { channel })
        .await
        .expect("room_tip")
        .tip
        .expect("tip");
    assert_eq!((tip.epoch, tip.counter), (receipt.epoch, receipt.counter));

    // A non-durable burst after the last message must NOT move the
    // durable tip (it is the transcript tip, not the ring back).
    for _ in 0..8 {
        client
            .publish(stream_chunk(channel, b"\x01\x02\x03"))
            .await
            .expect("publish chunk");
    }
    let tip_after_chunks = client
        .room_tip(RoomTipRequest { channel })
        .await
        .expect("room_tip")
        .tip
        .expect("tip");
    assert_eq!(
        (tip_after_chunks.epoch, tip_after_chunks.counter),
        (receipt.epoch, receipt.counter),
        "stream chunks do not move the durable tip"
    );

    // Rooms are isolated: another room's tip is still None.
    let other = client
        .room_tip(RoomTipRequest {
            channel: RoomId::from_u128(0x712),
        })
        .await
        .expect("room_tip");
    assert_eq!(other.tip, None);

    daemon.stop().await;
}

/// The honest perf evidence (card a1562dbc): on a room thousands of
/// events deep, the typed tip probe answers the same cursor as the old
/// `Inbox { since: None, limit: 1 }` shape — which forces a full-room
/// replay inside the daemon — at a fraction of the cost. Real measured
/// numbers are printed for the PR body; the hard structural O(1) gate
/// (zero scan calls) lives in `airc-bus/tests/durable_tip.rs`.
#[tokio::test]
async fn deep_room_tip_probe_matches_inbox_scan_and_is_cheaper() {
    const DEEP: usize = 5_000;
    const PROBES: u32 = 20;

    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());
    let channel = RoomId::from_u128(0xd33b);

    let receipt = publish_n(&client, channel, DEEP).await;

    // Old path: inbox with no cursor — daemon replays all DEEP events
    // and returns the newest 1.
    let scan_start = Instant::now();
    let mut scan_newest = None;
    for _ in 0..PROBES {
        let page = client
            .inbox(InboxRequest {
                since: None,
                channel: Some(channel),
                limit: Some(1),
            })
            .await
            .expect("inbox");
        scan_newest = page.newest;
    }
    let scan_elapsed = scan_start.elapsed();

    // New path: the typed O(1) probe.
    let probe_start = Instant::now();
    let mut probe_tip = None;
    for _ in 0..PROBES {
        probe_tip = client
            .room_tip(RoomTipRequest { channel })
            .await
            .expect("room_tip")
            .tip;
    }
    let probe_elapsed = probe_start.elapsed();

    // Same answer…
    let scan_newest = scan_newest.expect("scan newest");
    let probe_tip = probe_tip.expect("probe tip");
    assert_eq!(probe_tip, scan_newest, "probe and scan agree on the tip");
    assert_eq!(
        (probe_tip.epoch, probe_tip.counter),
        (receipt.epoch, receipt.counter),
        "and both are the last publish receipt"
    );

    let scan_us = scan_elapsed.as_micros() / u128::from(PROBES);
    let probe_us = probe_elapsed.as_micros() / u128::from(PROBES);
    eprintln!(
        "card a1562dbc: room depth {DEEP}, {PROBES} probes each — \
         inbox(limit:1) full-room scan: {scan_us} µs/op; \
         room_tip probe: {probe_us} µs/op"
    );

    // …at strictly lower cost. The scan materializes DEEP envelopes per
    // call; the probe reads one ring slot / one indexed row. A generous
    // margin (2x) keeps this stable under CI scheduling noise — locally
    // the gap is orders of magnitude.
    assert!(
        probe_elapsed * 2 < scan_elapsed,
        "tip probe ({probe_us} µs/op) should be far cheaper than the \
         full-room inbox scan ({scan_us} µs/op) on a {DEEP}-deep room"
    );

    daemon.stop().await;
}

/// Restart shape: the tip must come from the durable tier's index when
/// the hot ring is empty (fresh daemon, history only in SQLite) — the
/// reconnect-watermark case the probe exists for.
#[tokio::test]
async fn room_tip_survives_daemon_restart_from_sqlite_index() {
    let home = tempfile::TempDir::new().expect("tempdir");
    let db_path = home.path().join("events.sqlite");
    let channel = RoomId::from_u128(0x4e57);

    let build = |home_path: PathBuf, db: PathBuf| async move {
        let peer_id = PeerId::new();
        let keypair = PeerKeypair::generate();
        let registry = PeerKeyRegistry::new();
        registry
            .enrol(peer_id, 0, keypair.public_bytes())
            .expect("enrol self");
        let coordinator: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
        Arc::new(
            DaemonState::build(
                peer_id,
                keypair,
                Arc::new(registry),
                VerificationPolicy::Strict,
                home_path,
                &db,
                coordinator,
                DaemonRuntimeInfo::unknown(),
            )
            .await
            .expect("build daemon state"),
        )
    };

    // First daemon lifetime: publish, capture the receipt, stop.
    let socket1 = unique_socket();
    let state1 = build(home.path().to_path_buf(), db_path.clone()).await;
    let s1 = socket1.clone();
    let h1 = tokio::spawn(async move {
        let _ = run(state1, s1).await;
    });
    for _ in 0..200 {
        if socket1.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let client1 = DaemonClient::new(socket1.clone());
    let receipt = publish_n(&client1, channel, 25).await;
    // Persistence barrier: write-behind is async, so poll the durable
    // tier's own index until the last receipt is on disk before the
    // restart — never a blind sleep.
    {
        use airc_bus::DurableSink;
        let probe_sink = airc_store::SqliteDurableSink::open_path(&db_path)
            .await
            .expect("open probe sink");
        let mut persisted = false;
        for _ in 0..400 {
            if let Some(cursor) = probe_sink.head_cursor(channel).await.expect("head_cursor") {
                if (cursor.seq.epoch, cursor.seq.counter) == (receipt.epoch, receipt.counter) {
                    persisted = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(persisted, "write-behind never flushed the last publish");
    }
    client1.stop().await.expect("stop daemon 1");
    let _ = tokio::time::timeout(Duration::from_secs(3), h1).await;

    // Second lifetime over the SAME ORM: ring is empty, the tip must
    // come from the SQLite index.
    let socket2 = unique_socket();
    let state2 = build(home.path().to_path_buf(), db_path.clone()).await;
    let s2 = socket2.clone();
    let h2 = tokio::spawn(async move {
        let _ = run(state2, s2).await;
    });
    for _ in 0..200 {
        if socket2.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let client2 = DaemonClient::new(socket2.clone());
    let tip = client2
        .room_tip(RoomTipRequest { channel })
        .await
        .expect("room_tip")
        .tip
        .expect("tip persisted across restart");
    assert_eq!(
        (tip.epoch, tip.counter, tip.event_id),
        (receipt.epoch, receipt.counter, receipt.event_id),
        "post-restart tip is the persisted durable head"
    );

    client2.stop().await.expect("stop daemon 2");
    let _ = tokio::time::timeout(Duration::from_secs(3), h2).await;
}
