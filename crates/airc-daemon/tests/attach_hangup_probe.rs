//! Card e28889cc — an attach stream whose client hung up is released by
//! the daemon WITHOUT waiting for the channel's next event.
//!
//! The bug this pins (measured 2026-09-12 on one node): `stream_attach`
//! selected only on shutdown and the router stream, never on the
//! client's half of the socket. A subscriber that closed — a core that
//! died at a deploy, a citizen re-opening on a membership epoch — left
//! its daemon-side socket open until the next event on that channel
//! tripped EPIPE on the write. On a quiet room that is never: 4,190
//! sockets on the daemon, ~3,500 with no peer, climbing at every core
//! restart toward the descriptor wall the whole node then hit.
//!
//! The test model IS the production model: a real `DaemonState` on a
//! Unix socket, driven by the real `DaemonClient` (same harness as
//! `delivery_stats_probe.rs`). The receipt is the live-connection count
//! `Status` now reports — the same number `airc status` prints.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_core::{PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::codec::read_frame;
use airc_ipc::request::{AttachRequest, AttachStart};
use airc_ipc::response::Response;
use airc_ipc::DaemonClient;
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use tokio::task::JoinHandle;

struct TestDaemon {
    socket: PathBuf,
    _state: Arc<DaemonState>,
    _handle: JoinHandle<()>,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    // Short /tmp path keeps us well under macOS SUN_LEN (104 bytes).
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-ahp-{}-{n}.sock", std::process::id()))
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
        _state: state,
        _handle: handle,
        _home: home,
    }
}

/// The count `Status` reports INCLUDES the status request's own
/// connection (connect-per-request), so "only the status call" reads
/// as 1 and "one attach stream + the status call" reads as 2.
async fn connections(client: &DaemonClient) -> usize {
    client
        .status()
        .await
        .expect("status")
        .connections
        .expect("a daemon built from this tree reports its connections")
}

/// what this catches (card e28889cc): the daemon releases an attach
/// stream when its client hangs up, on a channel that never speaks
/// again. Without the hang-up arm the count stays at 2 forever and the
/// bounded wait below fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_up_attach_stream_is_released_without_waiting_for_an_event() {
    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());
    assert_eq!(
        connections(&client).await,
        1,
        "baseline: the status call alone"
    );

    // A subscriber attaches to a room nobody will ever publish to and
    // reads the daemon's ack — the stream is live and registered.
    let room = RoomId::new();
    let mut stream = client
        .attach(AttachRequest::new(room, AttachStart::Live))
        .await
        .expect("attach");
    let ack: Option<Response> = read_frame(&mut stream).await.expect("read ack");
    assert!(matches!(ack, Some(Response::Ok)), "attach ack, got {ack:?}");
    assert_eq!(
        connections(&client).await,
        2,
        "the attach stream + the status call"
    );

    // The client hangs up. No event is published on `room` — the only
    // way the daemon can learn of this is by watching the socket.
    drop(stream);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let n = connections(&client).await;
        if n == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon still holds the hung-up attach stream after 3 s (connections = {n})"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// what this catches: the hang-up arm must not trip on a client that is
/// merely alive — an attach stream whose client stays connected keeps
/// its subscription (and its connection) for as long as it is held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_attach_stream_is_kept_while_its_client_holds_it() {
    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(AttachRequest::new(RoomId::new(), AttachStart::Live))
        .await
        .expect("attach");
    let ack: Option<Response> = read_frame(&mut stream).await.expect("read ack");
    assert!(matches!(ack, Some(Response::Ok)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        connections(&client).await,
        2,
        "held stream + the status call"
    );
    drop(stream);
}
