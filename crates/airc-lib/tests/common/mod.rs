//! Shared in-process daemon fixture for SDK integration tests.
//!
//! The owner-core model: same-machine delivery is ONE daemon's
//! in-memory router over ONE SQLite ORM — never a `frames.jsonl`
//! file wire. These helpers spin that daemon up **in-process** (real
//! `DaemonState`, real Unix socket) and attach real `Airc` SDK
//! handles to it. That is the production path, exercised without
//! spawning the `airc` binary, so consumer surfaces (diagnostics,
//! bridge, PR-observe, work events, WebRTC signaling) get tested the
//! way they actually run.
//!
//! Shared by multiple test binaries; not every helper is used by
//! each, hence `dead_code` is allowed.
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
    // Short /tmp path stays well under macOS SUN_LEN (104 bytes); a
    // TempDir-rooted socket can blow past it.
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-it-{}-{n}.sock", std::process::id()))
}

/// A live in-process daemon: a real router + SQLite ORM behind a Unix
/// socket. The owning task is aborted on drop so tests never leak it.
pub struct DaemonFixture {
    pub socket: PathBuf,
    state: Arc<DaemonState>,
    handle: JoinHandle<()>,
    _home: TempDir,
}

impl DaemonFixture {
    pub async fn start() -> Self {
        let home = TempDir::new().expect("daemon home");
        let socket = unique_socket();
        let (state, handle) = Self::spawn_on(home.path(), socket.clone()).await;
        Self {
            socket,
            state,
            handle,
            _home: home,
        }
    }

    /// Build a `DaemonState` over `home/events.sqlite` and serve it on
    /// `socket`, returning the state + listener task once the socket is
    /// bound. Restart reuses the SAME home (durable transcript persists)
    /// + the SAME socket so attached clients reconnect transparently.
    async fn spawn_on(
        home: &std::path::Path,
        socket: PathBuf,
    ) -> (Arc<DaemonState>, JoinHandle<()>) {
        let db_path = home.join("events.sqlite");
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
                home.to_path_buf(),
                &db_path,
                coordinator,
                DaemonRuntimeInfo::unknown(),
            )
            .await
            .expect("build daemon state"),
        );
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
        (state, handle)
    }

    /// Faithful daemon restart on the same socket + durable db. Fires the
    /// shutdown notifier (graceful `airc stop` equivalent) so the accept
    /// loop AND every live connection handler return — that's what closes
    /// an attached client's stream and makes it reconnect. Aborting only
    /// the accept task would leave connection tasks alive (and clients
    /// none the wiser), which is not a real restart.
    pub async fn restart(&mut self) {
        self.state.shutdown.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut self.handle).await;
        let _ = std::fs::remove_file(&self.socket);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (state, handle) = Self::spawn_on(self._home.path(), self.socket.clone()).await;
        self.state = state;
        self.handle = handle;
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        self.handle.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// One simulated machine: a daemon plus a single shared mesh /
/// coordinator root. Every scope attached here derives the same mesh
/// identity, hence the same `RoomId` for a given channel name, so two
/// scopes converge through the daemon — exactly like two tabs under
/// one `$HOME`. No process-global env mutation, so tests stay
/// parallel-safe.
pub struct Machine {
    pub daemon: DaemonFixture,
    root: TempDir,
}

impl Machine {
    pub async fn boot() -> Self {
        Self {
            daemon: DaemonFixture::start().await,
            root: TempDir::new().expect("machine root"),
        }
    }

    /// Hard-restart this machine's daemon (same socket + durable db).
    pub async fn restart_daemon(&mut self) {
        self.daemon.restart().await;
    }

    /// Attach a new scope ("tab"/agent) to this machine's daemon.
    pub async fn attach(&self, scope: &str) -> Airc {
        let home = self.root.path().join(scope);
        Airc::attach_with_wire_root_for_test(
            home,
            self.root.path().to_path_buf(),
            &self.daemon.socket,
        )
        .await
        .expect("attach scope to daemon")
    }

    /// Attach one agent and join `room` — the single-participant setup.
    pub async fn solo(&self, room: &str) -> Airc {
        let airc = self.attach("solo").await;
        airc.join(room).await.expect("solo joins room");
        airc
    }

    /// Attach two mutually-trusting agents (alice, bob) and join both to
    /// `room` — the two-participant setup most consumer round-trips need.
    pub async fn pair_in(&self, room: &str) -> (Airc, Airc) {
        let alice = self.attach("alice").await;
        let bob = self.attach("bob").await;
        trust(&alice, &bob).await;
        alice.join(room).await.expect("alice joins room");
        bob.join(room).await.expect("bob joins room");
        (alice, bob)
    }
}

/// Mutually trust two scopes (each enrols the other's pinned key) so
/// signed-frame verification passes on receive.
pub async fn trust(a: &Airc, b: &Airc) {
    let a_spec: PeerSpec = a.peer_spec().parse().expect("a peer spec");
    let b_spec: PeerSpec = b.peer_spec().parse().expect("b peer spec");
    a.add_peer(b_spec).await.expect("a trusts b");
    b.add_peer(a_spec).await.expect("b trusts a");
}
