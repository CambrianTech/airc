//! In-process owner-core daemon for the embedding integration tests.
//!
//! The runtime embedding surface (`src/agent.rs`) depends only on
//! `airc-lib` and reaches the substrate purely through `Airc::attach`.
//! Same-machine delivery, though, is the one machine daemon's job — so
//! these tests bring that daemon up in-process (the airc install
//! provides it in production) and attach consumers to it. The
//! substrate deps below are TEST HARNESS only.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_core::PeerId;
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_lib::{Airc, PeerSpec};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use tempfile::TempDir;
use tokio::task::JoinHandle;

fn unique_socket() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-ecs-{}-{n}.sock", std::process::id()))
}

/// One simulated machine: a daemon plus a shared mesh/coordinator root.
/// Every consumer attached here derives the same mesh identity, so two
/// agents converge through the daemon — like two tabs under one `$HOME`.
pub struct Machine {
    socket: PathBuf,
    handle: JoinHandle<()>,
    _daemon_home: TempDir,
    root: TempDir,
}

impl Machine {
    pub async fn boot() -> Self {
        let home = TempDir::new().expect("daemon home");
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
        Self {
            socket,
            handle,
            _daemon_home: home,
            root: TempDir::new().expect("machine root"),
        }
    }

    /// Attach a consumer `Airc` (a "tab"/agent) to this machine's daemon.
    pub async fn attach(&self, scope: &str) -> Airc {
        let home = self.root.path().join(scope);
        Airc::attach_with_wire_root_for_test(home, self.root.path().to_path_buf(), &self.socket)
            .await
            .expect("attach consumer to daemon")
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.handle.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Mutually trust two consumers so signed-frame verification passes.
pub async fn trust(a: &Airc, b: &Airc) {
    let a_spec: PeerSpec = a.peer_spec().parse().expect("a peer spec");
    let b_spec: PeerSpec = b.peer_spec().parse().expect("b peer spec");
    a.add_peer(b_spec).await.expect("a trusts b");
    b.add_peer(a_spec).await.expect("b trusts a");
}
