//! Card 4b6a0ffa (#33) — `route_endpoints` against the LIVE daemon:
//! the typed probe a short-lived CLI publisher (`airc registry sync`)
//! uses to read back the daemon's dialable endpoints instead of
//! publishing an endpoint-less beacon (or advertising its own
//! about-to-die listener port).
//!
//! The test model IS the production model: a real `DaemonState` over a
//! real SQLite ORM on a Unix socket, driven by the real `DaemonClient`.
//!
//! Mutation checks (both verified at authoring time):
//!   - removing the `Request::RouteEndpoints` dispatch arm fails to
//!     compile (exhaustive match) — the compiler is the pin;
//!   - serving an empty vec instead of `state.route_endpoints` fails
//!     `daemon_serves_recorded_route_endpoints`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_core::PeerId;
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::{DaemonClient, IpcRouteEndpoint};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use tokio::task::JoinHandle;

/// A live daemon on a Unix socket, owning a real router + SQLite ORM.
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
    PathBuf::from(format!("/tmp/airc-rep-{}-{n}.sock", std::process::id()))
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

impl TestDaemon {
    async fn stop(self) {
        let _ = DaemonClient::new(self.socket.clone()).stop().await;
        let _ = tokio::time::timeout(Duration::from_secs(3), self.handle).await;
    }
}

/// A daemon that has not bound a listener serves an EMPTY endpoint
/// list — "up but not dialable", which callers must treat exactly
/// like "no daemon" for publish decisions.
#[tokio::test]
async fn daemon_without_listener_serves_empty_endpoints() {
    let daemon = start_daemon().await;
    let client = DaemonClient::new(daemon.socket.clone());

    let response = client.route_endpoints().await.expect("probe must answer");
    assert!(
        response.endpoints.is_empty(),
        "no registry glue ran — endpoints must be empty, got {:?}",
        response.endpoints
    );

    daemon.stop().await;
}

/// Endpoints recorded by the registry glue come back through the typed
/// probe byte-for-byte — the read-back path `airc registry sync` uses.
#[tokio::test]
async fn daemon_serves_recorded_route_endpoints() {
    let daemon = start_daemon().await;
    let recorded = vec![
        IpcRouteEndpoint::LanTcp {
            addr: "10.0.0.2:7717".parse().expect("valid socket addr"),
        },
        IpcRouteEndpoint::Relay {
            url: "https://relay.example.test".to_string(),
        },
    ];
    // The same write the daemon's registry glue performs after
    // `listen_lan` succeeds (crates/airc-cli/src/commands.rs).
    *daemon.state.route_endpoints.write().await = recorded.clone();

    let client = DaemonClient::new(daemon.socket.clone());
    let response = client.route_endpoints().await.expect("probe must answer");
    assert_eq!(
        response.endpoints, recorded,
        "the daemon must serve exactly the endpoints the registry glue recorded"
    );

    daemon.stop().await;
}
