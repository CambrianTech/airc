//! Daemon's shared state — peer identity, registry, the owner-core
//! event router, the coordinator store, shutdown notifier.
//!
//! `DaemonState` is constructed once at startup and passed (via Arc) to
//! every per-connection handler. Handlers read fields directly; the
//! substrate enforces its own internal locking.
//!
//! Owner-core model (§3 of `AIRC-EVENT-SERVER.md`): the daemon owns ONE
//! [`EventRouter`] backed by ONE machine ORM ([`SqliteDurableSink`] +
//! persisted epoch). Same-machine delivery is the router's in-memory
//! fan-out — there is no `frames.jsonl`, no `LocalFsAdapter`, no
//! per-wire subscriber task, no `broadcast` live channel. The durable
//! transcript lives in the router's sink; the `coordinator_store` keeps
//! only the non-transcript rows (subscriptions, beacons, mesh identity).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, Notify, RwLock};

use airc_bus::{BusError, Clock, EventRouter, RouterConfig, SeqSource, SystemClock};
use airc_core::PeerId;
use airc_ipc::IpcRouteEndpoint;
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, SqliteDurableSink};

/// Everything a daemon needs at runtime. Cheap to clone via Arc; the
/// underlying handles (registry, router, store) do their own interior
/// locking.
pub struct DaemonState {
    pub peer_id: PeerId,
    pub keypair: PeerKeypair,
    pub registry: Arc<PeerKeyRegistry>,
    pub policy: VerificationPolicy,
    /// Machine-account home the daemon owns. One daemon per machine
    /// account → one home, one ORM, one identity, one room set.
    pub home: PathBuf,
    /// When the daemon started — used for the Status uptime field.
    pub started_at: Instant,
    /// The owner-core engine: sharded router + hot ring + generational
    /// cursor + write-behind to the machine ORM. The single source of
    /// truth for same-machine delivery and durable transcript.
    pub router: EventRouter,
    /// Non-transcript durable rows: subscriptions (room membership),
    /// beacons (presence), mesh identity. The durable *event* transcript
    /// is the router's sink, not this store.
    pub coordinator_store: Arc<dyn EventStore>,
    /// Trust roots already watched by this daemon (cross-machine peer
    /// trust; same-machine publishers are in-process trusted).
    pub trusted_roots: Mutex<HashMap<PathBuf, ()>>,
    /// Notified when the daemon should stop accepting + exit cleanly.
    pub shutdown: Notify,
    /// Runtime metadata reported through IPC status so clients can
    /// replace stale daemons after updates.
    pub runtime: DaemonRuntimeInfo,
    /// Card 4b6a0ffa (#33): the dialable endpoints this daemon
    /// currently advertises in its account-registry beacon. Written by
    /// the registry glue after it binds its LAN listener; served to
    /// clients via `Request::RouteEndpoints` so a short-lived CLI
    /// publisher (`airc registry sync`) can publish the daemon's LIVE
    /// endpoints instead of an endpoint-less beacon. Empty = this
    /// daemon is not dialable (no listener bound).
    pub route_endpoints: RwLock<Vec<IpcRouteEndpoint>>,
}

impl DaemonState {
    /// Build the daemon state, opening the machine ORM and standing up
    /// the owner-core router against it.
    ///
    /// - `db_path` is the single machine-account event database
    ///   (`<machine_home>/events.sqlite`); the durable transcript and the
    ///   generational epoch both live here (one ORM, single writer).
    /// - `coordinator_store` holds subscriptions / beacons / identity.
    ///
    /// The epoch is bumped once here (per-start), so post-restart events
    /// sort strictly after pre-restart ones with no counter reissue
    /// (§3.8) — that's why `SeqSource::start` needs no durable-max rebuild.
    #[allow(clippy::too_many_arguments)]
    pub async fn build(
        peer_id: PeerId,
        keypair: PeerKeypair,
        registry: Arc<PeerKeyRegistry>,
        policy: VerificationPolicy,
        home: PathBuf,
        db_path: &Path,
        coordinator_store: Arc<dyn EventStore>,
        runtime: DaemonRuntimeInfo,
    ) -> Result<Self, BusError> {
        let sink = Arc::new(SqliteDurableSink::open_path(db_path).await?);
        let epoch_store = sink.bump_epoch().await?;
        let seq = Arc::new(SeqSource::start(&epoch_store));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let router = EventRouter::new(RouterConfig::default(), clock, seq, sink);
        Ok(Self {
            peer_id,
            keypair,
            registry,
            policy,
            home,
            started_at: Instant::now(),
            router,
            coordinator_store,
            trusted_roots: Mutex::new(HashMap::new()),
            shutdown: Notify::new(),
            runtime,
            route_endpoints: RwLock::new(Vec::new()),
        })
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

#[derive(Debug, Clone, Default)]
pub struct DaemonRuntimeInfo {
    pub ipc_protocol_version: Option<u32>,
    pub build_commit: Option<String>,
    pub build_branch: Option<String>,
    pub executable: Option<String>,
}

impl DaemonRuntimeInfo {
    pub fn unknown() -> Self {
        Self::default()
    }
}
