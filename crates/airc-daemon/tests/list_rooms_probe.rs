//! #270/#241 — `list_rooms` against the LIVE daemon: the durable
//! subscribed-room registry served over IPC.
//!
//! Before this op, an attached client (continuum's nav, a TUI room
//! list) could only learn rooms by watching traffic — a rebooted
//! interface showed ONE room until each of the others happened to
//! speak (live-found 2026-07-31: Joel's interface down to a
//! single room). These tests pin the membership read: every subscribed
//! room is returned with its name and default flag, parted rooms are
//! excluded, and an empty registry is an empty list — never an error.
//!
//! The test model IS the production model: a real `DaemonState` over a
//! real coordinator store on a Unix socket, driven by the real
//! `DaemonClient` — the same shape as `room_tip_probe.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_core::{PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::DaemonClient;
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore, StoredSubscription};
use tokio::task::JoinHandle;

struct TestDaemon {
    socket: PathBuf,
    handle: JoinHandle<()>,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    // Short /tmp path keeps us well under macOS SUN_LEN (104 bytes).
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-lrp-{}-{n}.sock", std::process::id()))
}

async fn start_daemon(coordinator: Arc<dyn EventStore>) -> TestDaemon {
    let home = tempfile::TempDir::new().expect("tempdir");
    let db_path = home.path().join("events.sqlite");
    let peer_id = PeerId::new();
    let keypair = PeerKeypair::generate();
    let registry = PeerKeyRegistry::new();
    registry
        .enrol(peer_id, 0, keypair.public_bytes())
        .expect("enrol self");
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

fn sub(name: &str, room: u128, is_default: bool, parted: bool) -> StoredSubscription {
    StoredSubscription {
        channel_name: name.to_string(),
        room_id: RoomId::from_u128(room),
        wire: String::new(),
        joined_at_ms: 1_700_000_000_000 + room as u64,
        is_default,
        parted,
    }
}

// what this catches (#270/#241): the membership read itself — every
// non-parted subscription comes back with its NAME, room id, and
// default flag straight from the coordinator store, and a parted room
// is excluded (the store remembers it only so auto-restore doesn't
// resurrect an explicit leave; serving it here would re-grow a tab the
// user closed). If this drifts, nav falls back to inferring rooms from
// traffic and the one-visible-room symptom returns.
#[tokio::test]
async fn list_rooms_serves_the_registry_and_excludes_parted() {
    let coordinator = Arc::new(InMemoryEventStore::new());
    coordinator
        .replace_subscriptions(vec![
            sub("general", 0x1, true, false),
            sub("project-x", 0x2, false, false),
            sub("old-experiment", 0x3, false, true), // explicitly left
        ])
        .await
        .expect("seed subscriptions");

    let daemon = start_daemon(coordinator).await;
    let client = DaemonClient::new(daemon.socket.clone());

    let mut rooms = client.list_rooms().await.expect("list_rooms").rooms;
    rooms.sort_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(rooms.len(), 2, "parted room excluded: {rooms:?}");
    assert_eq!(rooms[0].name, "general");
    assert_eq!(rooms[0].room_id, RoomId::from_u128(0x1));
    assert!(rooms[0].is_default, "default flag survives the wire");
    assert_eq!(rooms[1].name, "project-x");
    assert!(!rooms[1].is_default);
    assert!(
        !rooms.iter().any(|r| r.name == "old-experiment"),
        "parted rooms never resurface"
    );

    daemon.stop().await;
}

// what this catches: a fresh scope (no subscriptions yet) answers an
// EMPTY list, never an error — the nav seed path must treat "no rooms"
// as a valid state and fall back to its bootstrap seed, not crash or
// retry-loop on a refused op.
#[tokio::test]
async fn empty_registry_is_an_empty_list_not_an_error() {
    let daemon = start_daemon(Arc::new(InMemoryEventStore::new())).await;
    let client = DaemonClient::new(daemon.socket.clone());
    let rooms = client.list_rooms().await.expect("list_rooms").rooms;
    assert!(rooms.is_empty());
    daemon.stop().await;
}
