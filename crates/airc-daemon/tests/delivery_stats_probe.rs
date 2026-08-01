//! #1306 slice 2 — `delivery_stats` against the LIVE daemon: the
//! per-peer delivery-ledger snapshot served over IPC.
//!
//! This is the delivery-truth read behind doctor's "last confirmed
//! delivery to X: N ago". The daemon serves whatever the host's
//! route-refresh loop wrote into `DaemonState::delivery_stats` (the
//! same single-writer wiring split as `connected_lan_peers` — the
//! daemon crate cannot reach the airc-lib ledger). These tests pin the
//! IPC round trip: rows written by the host come back verbatim, and an
//! empty snapshot (no cross-machine forward yet) is an empty list —
//! never an error.
//!
//! The test model IS the production model: a real `DaemonState` on a
//! Unix socket, driven by the real `DaemonClient` — the same shape as
//! `list_rooms_probe.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_core::PeerId;
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::{DaemonClient, IpcPeerDeliveryStats};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use tokio::task::JoinHandle;

struct TestDaemon {
    socket: PathBuf,
    state: Arc<DaemonState>,
    handle: JoinHandle<()>,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    // Short /tmp path keeps us well under macOS SUN_LEN (104 bytes).
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-dsp-{}-{n}.sock", std::process::id()))
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
        state,
        handle,
        _home: home,
    }
}

/// what this catches (#1306 slice 2): the delivery-truth round trip —
/// rows the host's refresh loop writes into `DaemonState` come back
/// verbatim over IPC, healthy and suspect alike. Doctor's "last
/// confirmed delivery" line renders exactly these fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivery_stats_round_trips_host_written_rows() {
    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());

    // Before any host write: empty, never an error.
    let empty = client.delivery_stats().await.expect("empty stats call");
    assert!(
        empty.peers.is_empty(),
        "no snapshot written yet must read as an empty list"
    );

    // The host's route-refresh loop publishes a snapshot: one confirmed
    // peer, one suspect (the half-open signature).
    let confirmed_peer = PeerId::from_u128(0xacc);
    let suspect_peer = PeerId::from_u128(0xbad);
    let rows = vec![
        IpcPeerDeliveryStats {
            peer_id: confirmed_peer,
            attempts: 7,
            acked: 7,
            attempts_since_ack: 0,
            last_attempt_ms: Some(1_000_000),
            last_ack_ms: Some(1_000_040),
            rtt_ema_ms: Some(40),
            suspect: false,
        },
        IpcPeerDeliveryStats {
            peer_id: suspect_peer,
            attempts: 3,
            acked: 0,
            attempts_since_ack: 3,
            last_attempt_ms: Some(2_000_000),
            last_ack_ms: None,
            rtt_ema_ms: None,
            suspect: true,
        },
    ];
    *daemon.state.delivery_stats.write().await = rows.clone();

    let served = client.delivery_stats().await.expect("stats call");
    assert_eq!(
        served.peers, rows,
        "the daemon must serve the host-written snapshot verbatim"
    );

    daemon.handle.abort();
}
