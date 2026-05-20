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
use std::sync::Arc;
use std::sync::RwLock;

use airc_core::{ClientId, PeerId, TranscriptEvent};
use airc_daemon::{peers_store, LocalIdentity};
use airc_protocol::{PeerKeyRegistry, VerificationPolicy};
use airc_store::{EventStore, SqliteEventStore};
use tokio::sync::{broadcast, Mutex};

use crate::error::AircError;
use crate::room::{self, Room};
use crate::route_health::TransportHealthSample;
use crate::route_policy::TransportKind;
use crate::transport::WireSubscriber;

const EVENTS_DB_FILENAME: &str = "events.sqlite";

/// Capacity of the live broadcast channel. Each consumer that calls
/// [`Airc::subscribe`] gets its own receiver; lagged receivers see
/// `BroadcastStreamRecvError::Lagged(n)` rather than silently miss
/// events — the operating doc's "no silent fallback" rule. Consumers
/// that need durable replay use `Airc::resume_from` against the store.
const LIVE_BROADCAST_CAPACITY: usize = 1024;

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
    pub(crate) identity: LocalIdentity,
    pub(crate) store: Arc<dyn EventStore>,
    pub(crate) registry: Arc<RwLock<PeerKeyRegistry>>,
    pub(crate) policy: VerificationPolicy,
    pub(crate) route_health: RwLock<Vec<TransportHealthSample>>,
    /// Per-wire background subscriber tasks. Spawned lazily on first
    /// `say`/`send`/`subscribe`/`page_recent` referencing the wire.
    /// Held in a Mutex so concurrent calls can't double-spawn.
    pub(crate) subscribers: Mutex<HashMap<PathBuf, WireSubscriber>>,
    /// Live event fan-out. Every event the subscribers append to the
    /// store is also forwarded here so consumers tailing via
    /// [`Airc::subscribe`] see it immediately.
    pub(crate) live_tx: broadcast::Sender<TranscriptEvent>,
}

impl Airc {
    /// Open or initialise an Airc handle at `<home>`. This call:
    ///   - Loads `<home>/identity.{key,json}` (generates if missing).
    ///   - Opens `<home>/events.sqlite` and applies any pending
    ///     event-store migrations.
    ///   - Loads `<home>/peers.json` into the in-memory trust registry.
    ///
    /// Production policy is always `VerificationPolicy::Strict` —
    /// unsigned frames are rejected. Use `open_with_policy` if a
    /// test harness needs a different stance.
    pub async fn open(home: impl Into<PathBuf>) -> Result<Self, AircError> {
        Self::open_with_policy(home, VerificationPolicy::Strict).await
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
        let identity = LocalIdentity::load_or_generate(&home)?;

        let store_path = home.join(EVENTS_DB_FILENAME);
        let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::open_path(&store_path).await?);

        let mut registry = PeerKeyRegistry::new();
        registry
            .enrol(identity.peer_id, 0, identity.keypair.public_bytes())
            .map_err(|e| AircError::Crypto(e.to_string()))?;
        for stored in peers_store::load(&home)? {
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
                home,
                identity,
                store,
                registry,
                policy,
                route_health: RwLock::new(vec![TransportHealthSample::healthy_direct(
                    TransportKind::LocalFs,
                )]),
                subscribers: Mutex::new(HashMap::new()),
                live_tx,
            }),
        })
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

    /// Replace the route-health view consumed by the resolver. Discovery
    /// and transport probes own this in production; tests and embedded
    /// harnesses can pin samples to prove route admission behavior.
    pub fn replace_transport_health(
        &self,
        samples: impl IntoIterator<Item = TransportHealthSample>,
    ) {
        let mut route_health = self
            .inner
            .route_health
            .write()
            .expect("route health lock poisoned");
        *route_health = samples.into_iter().collect();
    }

    /// Switch the current room to one derived from `name`. Same name
    /// on two peers yields the same channel UUID via UUIDv5, so they
    /// converge without exchanging the UUID out-of-band. Spawns a
    /// background subscriber on the new room's wire if one isn't
    /// already running, so subsequent `say`s land in the store.
    pub async fn join(&self, name: &str) -> Result<Room, AircError> {
        let room = Room::from_name(&self.inner.home, name)?;
        room::save(&self.inner.home, &room)?;
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(room)
    }

    /// Variant of [`join`] that overrides the per-home default wire
    /// dir. Used for shared-wire setups (local-fs tests where two
    /// processes on one machine tail the same `frames.jsonl`).
    /// Production users want [`join`].
    pub async fn join_with_wire(&self, name: &str, wire: PathBuf) -> Result<Room, AircError> {
        let mut room = Room::from_name(&self.inner.home, name)?;
        room.wire = wire;
        room::save(&self.inner.home, &room)?;
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(room)
    }

    /// Read the persisted current room. Returns the default room
    /// (synthesised on the fly, NOT persisted) if no `room.json`
    /// has been written yet.
    pub async fn current_room(&self) -> Result<Room, AircError> {
        Ok(room::load_or_default(&self.inner.home)?)
    }

    pub(crate) fn event_store(&self) -> &dyn EventStore {
        self.inner.store.as_ref()
    }
}
