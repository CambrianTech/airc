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

use airc_core::{PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_lib::{Airc, PeerSpec};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, SqliteEventStore};
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
    home: PathBuf,
    /// Present only when this fixture owns its home (standalone
    /// [`start`]); `None` when a [`Machine`] owns the shared root and
    /// lends it via [`start_in`], so the daemon and every scope resolve
    /// ONE machine-account home.
    _owned_home: Option<TempDir>,
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
            home: home.path().to_path_buf(),
            _owned_home: Some(home),
        }
    }

    /// Like [`start`], but roots the daemon at a caller-owned `home` so
    /// the daemon's coordinator store (`home/events.sqlite`) is the SAME
    /// on-disk machine-account sqlite attached scopes write their durable
    /// identity index to (`wire_root/events.sqlite`). Faithful to
    /// production `run_daemon`, where the daemon and every scope resolve
    /// one `machine_account_home/events.sqlite` — so a scope's identity
    /// write is visible to the daemon's `peer_identity_card` IPC read.
    pub async fn start_in(home: PathBuf) -> Self {
        let socket = unique_socket();
        let (state, handle) = Self::spawn_on(&home, socket.clone()).await;
        Self {
            socket,
            state,
            handle,
            home,
            _owned_home: None,
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
        // Production faithfulness (`run_daemon`, commands.rs): the
        // daemon's coordinator store is a Sqlite over the SAME
        // `home/events.sqlite` the router transcript uses AND that
        // attached scopes resolve as their coordinator
        // (`wire_root/events.sqlite`). A scope writes its identity index
        // there (`record_peer_identity_card`); the daemon reads it back
        // in `handle_peer_identity_card`. An `InMemoryEventStore` here is
        // disjoint from that on-disk file, so every daemon-side identity /
        // alias lookup returned `None` — the durable-index bug this fixes.
        let coordinator: Arc<dyn EventStore> = Arc::new(
            SqliteEventStore::open_path(&db_path)
                .await
                .expect("coordinator store"),
        );
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

    /// The daemon's owner-core router. Card 4132f48c tests build the
    /// production `RouterInboundBridge` against it, exactly like
    /// `run_daemon` does.
    pub fn router(&self) -> airc_bus::EventRouter {
        self.state.router.clone()
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
        let (state, handle) = Self::spawn_on(&self.home, self.socket.clone()).await;
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
        // ONE machine-account home: the daemon roots at the SAME `root`
        // every scope attaches against, so `root/events.sqlite` is the
        // single coordinator store the daemon writes/reads AND scopes
        // resolve as their `wire_root` — mirroring production, where the
        // daemon and every scope share `machine_account_home/events.sqlite`.
        let root = TempDir::new().expect("machine root");
        let daemon = DaemonFixture::start_in(root.path().to_path_buf()).await;
        Self { daemon, root }
    }

    /// Hard-restart this machine's daemon (same socket + durable db).
    pub async fn restart_daemon(&mut self) {
        self.daemon.restart().await;
    }

    /// The shared wire root every scope on this machine resolves —
    /// where presence beacons + the mesh-identity cache live
    /// (`<root>/events.sqlite`), i.e. the machine coordinator store.
    pub fn wire_root(&self) -> &std::path::Path {
        self.root.path()
    }

    /// Pin this machine's mesh identity (Operator-source, never
    /// re-resolved) into the shared coordinator store every attached
    /// scope resolves against. Call BEFORE the first `attach`/`join`
    /// so no scope ever falls through to the live gh/git resolver.
    /// See [`pin_identity`] for the shared-state class this kills.
    pub async fn pin_identity(&self, identity: &str) {
        let store = SqliteEventStore::open_path(&self.root.path().join("events.sqlite"))
            .await
            .expect("open machine coordinator store for identity pin");
        pin_identity(&store, identity).await;
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

/// Pin a mesh identity into `store` with an `Operator`-source cache
/// entry — trusted as-is, never expired, never re-resolved — so
/// identity-sensitive tests are hermetic. Shared-state class this
/// kills: the default mesh-identity resolver reads LIVE HOST STATE
/// shared by every parallel test — `gh api user` (network + gh auth,
/// 3s kill-deadline), `git config user.email`, and on total probe
/// failure the REAL `~/.airc/machine-id` — so its outcome varies with
/// suite load and box configuration, and a provisional (non-gh) result
/// re-probes `gh` on every later resolve. An Operator pin short-circuits
/// all of it: no shell-outs, no real-home writes, deterministic RoomId
/// derivation on any box.
pub async fn pin_identity(store: &dyn EventStore, identity: &str) {
    airc_lib::mesh_identity::save(
        store,
        &airc_lib::CachedIdentity {
            version: 1,
            identity: identity.to_string(),
            source: airc_lib::mesh_identity::Source::Operator,
            resolved_at_ms: 1,
            ttl_ms: airc_lib::mesh_identity::DEFAULT_TTL_MS,
        },
    )
    .await
    .expect("pin mesh identity");
}

/// Mutually trust two scopes (each enrols the other's pinned key) so
/// signed-frame verification passes on receive.
pub async fn trust(a: &Airc, b: &Airc) {
    let a_spec: PeerSpec = a.peer_spec().parse().expect("a peer spec");
    let b_spec: PeerSpec = b.peer_spec().parse().expect("b peer spec");
    a.add_peer(b_spec).await.expect("a trusts b");
    b.add_peer(a_spec).await.expect("b trusts a");
}

/// Put every scope in ONE room and return its id.
///
/// The first scope resolves `label` on its own account — minting the
/// room if this account has never used the name — and every other
/// scope joins THAT id.
///
/// Why this is a helper and not `scope.join(label)` per scope: a label
/// keys a room only WITHIN one account. Two scopes on two simulated
/// machines each joining `"room"` get two different rooms that merely
/// share a name, and every frame between them comes back
/// `Undeliverable { UnknownChannel }`. The id is the address, so a
/// test that means "these scopes are together" has to say so by id —
/// once, here, rather than correctly in each of a dozen call sites.
pub async fn same_room(label: &str, scopes: &[&Airc]) -> RoomId {
    let (first, rest) = scopes.split_first().expect("at least one scope");
    let room_id = first
        .join(label)
        .await
        .expect("first scope resolves the label on its own account")
        .channel;
    for scope in rest {
        scope
            .join_room_id(room_id, label)
            .await
            .expect("scope joins the SAME room by id");
    }
    room_id
}
