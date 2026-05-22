//! The `Airc` facade — primary entrypoint for consumer apps.
//!
//! Owns the substrate handles (identity, store, peer registry,
//! local-fs transport per room). Cheap to clone via inner `Arc`s.
//!
//! Lifecycle:
//!
//! ```no_run
//! # async fn run(home: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
//! use airc_lib::Airc;
//!
//! let airc = Airc::open(home).await?;
//! airc.join("project-x").await?;
//! airc.say("hello").await?;
//! let recent = airc.page_recent(10).await?;
//! for event in &recent {
//!     println!("{} → {}", event.peer_id, event.event_id);
//! }
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

use airc_core::{ClientId, PeerId, TranscriptEvent};
use airc_daemon::{peers_store, DaemonClient, LocalIdentity};
use airc_protocol::{PeerKeyRegistry, VerificationPolicy};
use airc_store::{EventStore, SqliteEventStore};
use airc_transport::LanTcpAdapter;
use tokio::sync::{broadcast, Mutex};

use crate::error::AircError;
use crate::join_context::JoinContext;
use crate::mesh_identity;
use crate::room::Room;
use crate::route::health::TransportHealthTable;
use crate::route::invite::{ImportedInviteTable, RouteEndpointTable};
use crate::route::TransportHealthSample;
use crate::subscriptions::{self, ChannelName, MeshIdentity, Subscription};
use crate::transport::{FrameSubscriber, WireSubscriber};
use crate::{coordinator, time};

const EVENTS_DB_FILENAME: &str = "events.sqlite";

/// Capacity of the live broadcast channel. Each consumer that calls
/// [`Airc::subscribe`] gets its own receiver; lagged receivers see
/// `BroadcastStreamRecvError::Lagged(n)` rather than silently miss
/// events — the operating doc's "no silent fallback" rule. Consumers
/// that need durable replay use `Airc::resume_from` against the store.
const LIVE_BROADCAST_CAPACITY: usize = 1024;

fn machine_account_home(scope_home: &Path) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let normalized_home = home.canonicalize().unwrap_or(home);
        let normalized_scope = scope_home
            .canonicalize()
            .unwrap_or_else(|_| scope_home.to_path_buf());
        if normalized_scope.starts_with(&normalized_home) {
            return normalized_home.join(".airc");
        }
    }
    #[cfg(windows)]
    if let Some(userprofile) = std::env::var_os("USERPROFILE") {
        let userprofile = PathBuf::from(userprofile);
        let normalized_userprofile = userprofile.canonicalize().unwrap_or(userprofile);
        let normalized_scope = scope_home
            .canonicalize()
            .unwrap_or_else(|_| scope_home.to_path_buf());
        if normalized_scope.starts_with(&normalized_userprofile) {
            return normalized_userprofile.join(".airc");
        }
    }
    scope_home.to_path_buf()
}

async fn load_peer_registries(
    home: &Path,
    wire_root: &Path,
) -> Result<Vec<peers_store::StoredPeer>, AircError> {
    let mut peers = peers_store::load(home).await?;
    if wire_root != home {
        peers.extend(peers_store::load(wire_root).await?);
    }
    Ok(peers)
}

/// In-process AIRC handle. Holds identity, store, per-room
/// signed-local-fs transports, and a background subscriber per wire
/// that converts received `Frame`s into `TranscriptEvent`s and
/// appends them to the durable store.
///
/// `Clone` is cheap (just an Arc bump). Clones share the SAME
/// subscriber set + live broadcast channel — call `.clone()` to
/// pass the handle into a spawned task while keeping the parent's
/// `subscribe()` stream live.
///
/// Lifecycle:
///   - `Airc::open` initialises identity + store + peer registry.
///   - `Airc::join(name)` / `Airc::say(text)` lazily start a
///     subscriber on the room's wire if one isn't already running.
///   - Consumers wanting live push call `Airc::subscribe()` and
///     get a `Stream<Item = TranscriptEvent>`.
#[derive(Clone)]
pub struct Airc {
    pub(crate) inner: Arc<AircInner>,
}

pub(crate) struct AircInner {
    pub(crate) home: PathBuf,
    pub(crate) wire_root: PathBuf,
    pub(crate) identity: LocalIdentity,
    pub(crate) store: Arc<dyn EventStore>,
    pub(crate) coordinator_store: Arc<dyn EventStore>,
    pub(crate) daemon_client: Option<Arc<DaemonClient>>,
    pub(crate) registry: Arc<RwLock<PeerKeyRegistry>>,
    pub(crate) policy: VerificationPolicy,
    pub(crate) route_health: RwLock<TransportHealthTable>,
    pub(crate) route_endpoints: RwLock<RouteEndpointTable>,
    pub(crate) imported_invites: RwLock<ImportedInviteTable>,
    pub(crate) lamport_clock: AtomicU64,
    pub(crate) lan_tcp: Mutex<Option<LanTcpAdapter>>,
    pub(crate) lan_subscriber: Mutex<Option<FrameSubscriber>>,
    /// Per-wire background subscriber tasks. Spawned lazily on first
    /// `say`/`send`/`subscribe`/`page_recent` referencing the wire.
    /// Held in a Mutex so concurrent calls can't double-spawn.
    pub(crate) subscribers: Mutex<HashMap<PathBuf, WireSubscriber>>,
    /// Live event fan-out. Every event the subscribers append to the
    /// store is also forwarded here so consumers tailing via
    /// [`Airc::subscribe`] see it immediately.
    pub(crate) live_tx: broadcast::Sender<TranscriptEvent>,
    /// Event IDs this Airc instance has already broadcast via
    /// [`live_tx`]. Consulted by the wire subscriber to avoid
    /// double-delivering a send that was already broadcast in-process
    /// by `append_sent_frame`. Bounded VecDeque used as a FIFO ring
    /// — older entries roll off once capacity is exceeded.
    ///
    /// Why a per-instance set rather than `(sender, client_id) ==
    /// self` detection: `client_id` is persisted in the singleton
    /// local identity row, so two processes on the same AIRC_HOME
    /// share it and the equality check would (incorrectly) suppress
    /// the cross-process peer's frames as "our own."
    pub(crate) recently_broadcast: std::sync::Mutex<std::collections::VecDeque<airc_core::EventId>>,
}

/// Capacity of the recently-broadcast ring. Sized to a multiple of
/// [`LIVE_BROADCAST_CAPACITY`] so even a maximally-lagging consumer
/// can't push valid events out of the set before they're delivered.
pub(crate) const RECENTLY_BROADCAST_CAPACITY: usize = LIVE_BROADCAST_CAPACITY * 4;

impl Airc {
    /// Open or initialise an Airc handle at `<home>`. This call:
    ///   - Loads `<home>/identity.{key,json}` (generates if missing).
    ///   - Opens `<home>/events.sqlite` and applies any pending
    ///     event-store migrations.
    ///   - Loads peer trust rows into the in-memory trust registry.
    ///
    /// Production policy is always `VerificationPolicy::Strict` —
    /// unsigned frames are rejected. Use `open_with_policy` if a
    /// test harness needs a different stance.
    pub async fn open(home: impl Into<PathBuf>) -> Result<Self, AircError> {
        Self::open_with_policy(home, VerificationPolicy::Strict).await
    }

    /// Attach to an already-running daemon. The handle still opens
    /// local identity/store state so consumers can inspect identity,
    /// room, and replay state through the same `Airc` facade, but
    /// send/inbox operations go through daemon IPC.
    pub async fn attach(
        home: impl Into<PathBuf>,
        socket: impl Into<PathBuf>,
    ) -> Result<Self, AircError> {
        let airc = Self::open(home).await?;
        Ok(airc.with_daemon_client(DaemonClient::new(socket.into())))
    }

    /// Variant of [`open`] that lets the caller pin the
    /// `VerificationPolicy`. The only legitimate non-Strict use is
    /// in-process tests that intentionally exercise unsigned paths.
    pub async fn open_with_policy(
        home: impl Into<PathBuf>,
        policy: VerificationPolicy,
    ) -> Result<Self, AircError> {
        let home: PathBuf = home.into();
        std::fs::create_dir_all(&home).map_err(airc_daemon::IdentityError::Io)?;
        let identity = LocalIdentity::load_or_generate(&home).await?;
        let wire_root = machine_account_home(&home);
        std::fs::create_dir_all(&wire_root).map_err(airc_daemon::IdentityError::Io)?;
        peers_store::add(
            &wire_root,
            identity.peer_id,
            identity.keypair.public_bytes(),
        )
        .await?;

        let store_path = home.join(EVENTS_DB_FILENAME);
        let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::open_path(&store_path).await?);
        let coordinator_store_path = wire_root.join(EVENTS_DB_FILENAME);
        let coordinator_store: Arc<dyn EventStore> =
            Arc::new(SqliteEventStore::open_path(&coordinator_store_path).await?);

        let mut registry = PeerKeyRegistry::new();
        registry
            .enrol(identity.peer_id, 0, identity.keypair.public_bytes())
            .map_err(|e| AircError::Crypto(e.to_string()))?;
        let mut enrolled = vec![identity.peer_id];
        for stored in load_peer_registries(&home, &wire_root).await? {
            if enrolled.contains(&stored.peer_id) {
                continue;
            }
            enrolled.push(stored.peer_id);
            registry
                .enrol(
                    stored.peer_id,
                    0,
                    stored
                        .pubkey_bytes()
                        .map_err(|e| AircError::Crypto(e.to_string()))?,
                )
                .map_err(|e| AircError::Crypto(e.to_string()))?;
        }
        let registry = Arc::new(RwLock::new(registry));
        let (live_tx, _) = broadcast::channel(LIVE_BROADCAST_CAPACITY);

        Ok(Self {
            inner: Arc::new(AircInner {
                wire_root,
                home,
                identity,
                store,
                coordinator_store,
                daemon_client: None,
                registry,
                policy,
                route_health: RwLock::new(TransportHealthTable::local_default()),
                route_endpoints: RwLock::new(RouteEndpointTable::default()),
                imported_invites: RwLock::new(ImportedInviteTable::default()),
                lamport_clock: AtomicU64::new(0),
                lan_tcp: Mutex::new(None),
                lan_subscriber: Mutex::new(None),
                subscribers: Mutex::new(HashMap::new()),
                live_tx,
                recently_broadcast: std::sync::Mutex::new(
                    std::collections::VecDeque::with_capacity(RECENTLY_BROADCAST_CAPACITY),
                ),
            }),
        })
    }

    /// Record `event_id` as broadcast in-process so the wire
    /// subscriber's later re-read doesn't double-deliver. Returns
    /// `true` if the event was added (not already present).
    pub(crate) fn mark_broadcast(&self, event_id: airc_core::EventId) -> bool {
        let mut ring = match self.inner.recently_broadcast.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if ring.contains(&event_id) {
            return false;
        }
        if ring.len() >= RECENTLY_BROADCAST_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(event_id);
        true
    }

    /// Return the home directory backing this handle.
    pub fn home(&self) -> &Path {
        &self.inner.home
    }

    /// Return the local peer's stable identifier.
    pub fn peer_id(&self) -> PeerId {
        self.inner.identity.peer_id
    }

    /// Return the per-session client identifier.
    pub fn client_id(&self) -> ClientId {
        self.inner.identity.client_id
    }

    pub fn is_daemon_attached(&self) -> bool {
        self.inner.daemon_client.is_some()
    }

    fn with_daemon_client(&self, client: DaemonClient) -> Self {
        let inner = AircInner {
            home: self.inner.home.clone(),
            wire_root: self.inner.wire_root.clone(),
            identity: self.inner.identity.clone(),
            store: self.inner.store.clone(),
            coordinator_store: self.inner.coordinator_store.clone(),
            daemon_client: Some(Arc::new(client)),
            registry: self.inner.registry.clone(),
            policy: self.inner.policy,
            route_health: RwLock::new(TransportHealthTable::local_default()),
            route_endpoints: RwLock::new(RouteEndpointTable::default()),
            imported_invites: RwLock::new(ImportedInviteTable::default()),
            lamport_clock: AtomicU64::new(self.inner.lamport_clock.load(Ordering::Relaxed)),
            lan_tcp: Mutex::new(None),
            lan_subscriber: Mutex::new(None),
            subscribers: Mutex::new(HashMap::new()),
            live_tx: self.inner.live_tx.clone(),
            recently_broadcast: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(
                RECENTLY_BROADCAST_CAPACITY,
            )),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Replace the route-health view consumed by the resolver.
    /// Discovery and transport probes own this in production; tests
    /// and embedded harnesses can pin samples to prove route
    /// admission behavior.
    pub fn replace_transport_health(
        &self,
        samples: impl IntoIterator<Item = TransportHealthSample>,
    ) -> Result<(), AircError> {
        let mut route_health = self
            .inner
            .route_health
            .write()
            .map_err(|_| AircError::Route("route health lock poisoned".to_string()))?;
        route_health.replace(samples);
        Ok(())
    }

    /// Snapshot the current route-health samples. Consumers can use
    /// this for diagnostics without reaching into resolver internals.
    pub fn transport_health(&self) -> Result<Vec<TransportHealthSample>, AircError> {
        self.inner
            .route_health
            .read()
            .map_err(|_| AircError::Route("route health lock poisoned".to_string()))
            .map(|table| table.samples())
    }

    pub(crate) fn upsert_transport_health(
        &self,
        sample: TransportHealthSample,
    ) -> Result<(), AircError> {
        let mut route_health = self
            .inner
            .route_health
            .write()
            .map_err(|_| AircError::Route("route health lock poisoned".to_string()))?;
        route_health.upsert(sample);
        Ok(())
    }

    /// Resolve the mesh identity for this scope, going through the
    /// cache. Single-flighted at the file level: the cache only
    /// re-resolves after [`crate::mesh_identity::DEFAULT_TTL_MS`] so
    /// concurrent callers don't hammer `gh`. See the module docs for
    /// the resolver chain.
    pub(crate) async fn mesh_identity(&self) -> Result<MeshIdentity, AircError> {
        let cached = mesh_identity::resolve(self.event_store()).await?;
        Ok(cached.as_mesh_identity())
    }

    pub(crate) async fn sync_account_peer_registry(&self) -> Result<(), AircError> {
        let peers = load_peer_registries(&self.inner.home, &self.inner.wire_root).await?;
        let mut registry = self
            .inner
            .registry
            .write()
            .map_err(|_| AircError::Crypto("registry lock poisoned".to_string()))?;
        for peer in peers {
            registry
                .enrol(
                    peer.peer_id,
                    0,
                    peer.pubkey_bytes()
                        .map_err(|e| AircError::Crypto(e.to_string()))?,
                )
                .map_err(|e| AircError::Crypto(e.to_string()))?;
        }
        Ok(())
    }

    async fn publish_presence(
        &self,
        identity: &MeshIdentity,
        set: &subscriptions::SubscriptionSet,
    ) -> Result<(), AircError> {
        let channels = set.channel_names().cloned().collect();
        let beacon = coordinator::beacon_now(
            self.inner.identity.peer_id,
            self.inner.home.clone(),
            channels,
            std::process::id(),
            time::now_ms()?,
        );
        coordinator::publish_store(self.coordinator_store(), identity, &beacon).await?;
        Ok(())
    }

    /// Subscribe to `name` and make it the default channel for
    /// short-shape commands.
    pub async fn join(&self, name: &str) -> Result<Room, AircError> {
        let channel = ChannelName::new(name)?;
        let identity = self.mesh_identity().await?;
        let mut set = subscriptions::load_or_init(self.event_store()).await?;
        let subscription =
            set.subscribe_with_wire_root(&self.inner.wire_root, &identity, channel.clone())?;
        set.set_default(channel)?;
        subscriptions::save(self.event_store(), &set).await?;
        self.publish_presence(&identity, &set).await?;
        let room = subscription.as_room();
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(room)
    }

    /// Subscribe this scope to the default account context:
    /// `#general` plus the repository owner channel inferred from
    /// `cwd` when the caller is inside a Git checkout.
    ///
    /// This is the Rust substrate for bare `airc join`. It creates
    /// missing local subscriptions idempotently, preserves arbitrary
    /// user-created channels, and uses the account-wide local wire so
    /// scopes on the same OS account converge without manual pairing.
    pub async fn join_default_context(
        &self,
        cwd: impl AsRef<Path>,
    ) -> Result<Vec<Room>, AircError> {
        let context = JoinContext::from_cwd(cwd.as_ref());
        self.ensure_join_context(context).await
    }

    /// Subscribe to every channel in `context`, set its default, and
    /// start local subscribers for the resulting wires.
    ///
    /// Network bootstrap is intentionally not in this critical path:
    /// `airc join` is the local, deterministic act of entering the
    /// account mesh. Cross-machine publication/refresh belongs to a
    /// bounded coordinator task so gh/network latency can never make
    /// the public join command hang.
    pub async fn ensure_join_context(&self, context: JoinContext) -> Result<Vec<Room>, AircError> {
        let identity = self.mesh_identity().await?;
        let mut set = subscriptions::load_or_init(self.event_store()).await?;
        let mut rooms = Vec::new();

        for channel in context.channels {
            if set.parted.contains(&channel) {
                continue;
            }
            let subscription =
                set.subscribe_with_wire_root(&self.inner.wire_root, &identity, channel)?;
            rooms.push(subscription.as_room());
        }

        if set.subscribed.contains_key(&context.default) {
            set.set_default(context.default)?;
        }
        subscriptions::save(self.event_store(), &set).await?;
        self.publish_presence(&identity, &set).await?;

        for room in &rooms {
            self.ensure_wire_subscriber(&room.wire).await?;
        }

        Ok(rooms)
    }

    /// Variant of [`join`] that overrides the per-home default wire
    /// dir. Used for shared-wire setups (local-fs tests where two
    /// processes on one machine tail the same `frames.jsonl`).
    /// Production users want [`join`].
    pub async fn join_with_wire(&self, name: &str, wire: PathBuf) -> Result<Room, AircError> {
        let channel = ChannelName::new(name)?;
        let identity = self.mesh_identity().await?;
        let mut set = subscriptions::load_or_init(self.event_store()).await?;
        let subscription = Subscription::with_wire(&identity, channel.clone(), wire)?;
        set.parted.remove(&channel);
        set.subscribed.insert(channel.clone(), subscription.clone());
        set.set_default(channel)?;
        subscriptions::save(self.event_store(), &set).await?;
        self.publish_presence(&identity, &set).await?;
        let room = subscription.as_room();
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(room)
    }

    /// Read the default subscribed channel. Fresh scopes default to
    /// `#general` through the subscription set, using the resolved
    /// mesh identity so the `RoomId` is stable per Git/GitHub user.
    pub async fn current_room(&self) -> Result<Room, AircError> {
        let mut set = subscriptions::load_or_init(self.event_store()).await?;
        if let Some(subscription) = set.default_subscription() {
            return Ok(subscription.as_room());
        }

        let identity = self.mesh_identity().await?;
        let channel = ChannelName::new("general")?;
        let subscription =
            set.subscribe_with_wire_root(&self.inner.wire_root, &identity, channel.clone())?;
        set.set_default(channel)?;
        subscriptions::save(self.event_store(), &set).await?;
        self.publish_presence(&identity, &set).await?;
        Ok(subscription.as_room())
    }

    pub(crate) fn event_store(&self) -> &dyn EventStore {
        self.inner.store.as_ref()
    }

    pub(crate) fn coordinator_store(&self) -> &dyn EventStore {
        self.inner.coordinator_store.as_ref()
    }

    /// Load a named runtime consumer checkpoint from the durable
    /// store. This is for hooks/feeds/monitors that need replay
    /// state; it is intentionally store-backed so runtime delivery
    /// state does not sprawl into JSON sidecars.
    pub async fn load_runtime_cursor(
        &self,
        consumer_id: &str,
    ) -> Result<Option<airc_core::TranscriptCursor>, AircError> {
        Ok(self.inner.store.load_runtime_cursor(consumer_id).await?)
    }

    /// Persist a named runtime consumer checkpoint in the durable
    /// store.
    pub async fn save_runtime_cursor(
        &self,
        consumer_id: &str,
        cursor: &airc_core::TranscriptCursor,
    ) -> Result<(), AircError> {
        self.inner
            .store
            .save_runtime_cursor(consumer_id, cursor, time::now_ms()?)
            .await?;
        Ok(())
    }

    pub(crate) fn next_lamport(&self, wall_ms: u64) -> u64 {
        let mut current = self.inner.lamport_clock.load(Ordering::Relaxed);
        loop {
            let next = wall_ms.max(current.saturating_add(1));
            match self.inner.lamport_clock.compare_exchange(
                current,
                next,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }
}
