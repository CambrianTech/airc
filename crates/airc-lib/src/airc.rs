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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use airc_core::{ClientId, PeerId, TranscriptEvent};
use airc_identity::{IdentityError, LocalIdentity};
use airc_ipc::DaemonClient;
use airc_protocol::{IdentityAssertion, PeerKeyRegistry, VerificationPolicy};
use airc_store::peer_trust::TrustTier;
use airc_store::{EventStore, SqliteEventStore};
use airc_transport::{udp::UdpAdapter, LanTcpAdapter, RelayAdapter};
use airc_trust as peers_store;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};

use crate::broadcast_deduper::BroadcastDeduper;
use crate::error::AircError;
use crate::join_context::JoinContext;
use crate::mesh_identity;
use crate::room::Room;
use crate::route::health::TransportHealthTable;
use crate::route::invite::{ImportedInviteTable, RouteEndpointTable};
use crate::route::TransportHealthSample;
use crate::subscriptions::{self, ChannelName, MeshIdentity};
use crate::transport::FrameSubscriber;
use crate::webrtc_media::{IncomingTrack, IncomingTrackHandler, IncomingTrackRegistry};
use crate::{coordinator, time};

const EVENTS_DB_FILENAME: &str = "events.sqlite";

/// Capacity of the live broadcast channel. Each consumer that calls
/// [`Airc::subscribe`] gets its own receiver; lagged receivers see
/// `BroadcastStreamRecvError::Lagged(n)` rather than silently miss
/// events — the operating doc's "no silent fallback" rule. Consumers
/// that need durable replay use `Airc::resume_from` against the store.
const LIVE_BROADCAST_CAPACITY: usize = 1024;

/// The machine-account home (`$HOME/.airc`) that owns the singular
/// daemon + the one ORM for every scope under this user's home. Scopes
/// outside `$HOME` (CI temp dirs, isolated test roots) get their own
/// `scope_home` back — they are their own account boundary.
pub fn machine_account_home(scope_home: &Path) -> PathBuf {
    // Temp-rooted scopes are their own account boundary on EVERY
    // platform. On Linux/macOS that falls out of `/tmp` living outside
    // `$HOME`, but on Windows `%TEMP%` is
    // `C:\Users\<user>\AppData\Local\Temp` — INSIDE `%USERPROFILE%` —
    // so without this guard a tempdir scope (CI, hermetic tests)
    // resolves to the REAL machine account and leaks real-world state.
    // Caught live on the 5090 node (card b0a81c31):
    // `peer_record_count_requires_valid_json_files` started failing the
    // moment a real peer was enrolled on the machine, while virgin
    // boxes (and therefore CI) kept passing.
    //
    // Carve-out (sentinel on PR #1119): when HOME/USERPROFILE is ITSELF
    // temp-rooted, a test harness is simulating a machine account by
    // pointing the home at a TempDir (daemon_lifecycle does exactly
    // this) — its scopes legitimately share that simulated account, so
    // the temp guard must not fire. Real boxes never have a temp-rooted
    // home, so the b0a81c31 fix is unaffected.
    let temp = std::env::temp_dir();
    let normalized_temp = temp.canonicalize().unwrap_or(temp);
    let normalized_scope_for_temp = scope_home
        .canonicalize()
        .unwrap_or_else(|_| scope_home.to_path_buf());
    let home_var = std::env::var_os("HOME").map(PathBuf::from);
    let profile_var = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let any_home_is_temp_rooted = [home_var.as_ref(), profile_var.as_ref()]
        .into_iter()
        .flatten()
        .any(|h| {
            h.canonicalize()
                .unwrap_or_else(|_| h.clone())
                .starts_with(&normalized_temp)
        });
    if !any_home_is_temp_rooted && normalized_scope_for_temp.starts_with(&normalized_temp) {
        return scope_home.to_path_buf();
    }

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

pub(crate) async fn load_peer_registries(
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
///     get a `Stream<Item = Arc<TranscriptEvent>>`.
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
    pub(crate) registry: Arc<PeerKeyRegistry>,
    pub(crate) policy: VerificationPolicy,
    pub(crate) route_health: Arc<TransportHealthTable>,
    pub(crate) route_endpoints: Arc<RouteEndpointTable>,
    pub(crate) imported_invites: Arc<ImportedInviteTable>,
    pub(crate) lamport_clock: AtomicU64,
    pub(crate) lan_tcp: Mutex<Option<LanTcpAdapter>>,
    pub(crate) lan_subscriber: Mutex<Option<FrameSubscriber>>,
    pub(crate) relay: Mutex<Option<RelayAdapter>>,
    pub(crate) relay_subscriber: Mutex<Option<FrameSubscriber>>,
    pub(crate) udp: Mutex<Option<UdpAdapter>>,
    pub(crate) udp_subscriber: Mutex<Option<FrameSubscriber>>,
    /// Per-peer WebRTC DataChannel adapters, keyed by the remote
    /// peer id. Populated by `Airc::open_webrtc_to` /
    /// `Airc::accept_webrtc_offers` after a handshake completes.
    pub(crate) webrtc_channels:
        Mutex<HashMap<PeerId, airc_transport::webrtc_datachannel::WebRtcDataChannelAdapter>>,
    /// Per-peer WebRTC ingest subscriber tasks. Mirrors the
    /// per-wire `subscribers` map but keyed by peer because each
    /// WebRTC DataChannel is its own transport instance.
    pub(crate) webrtc_subscribers: Mutex<HashMap<PeerId, FrameSubscriber>>,
    /// Keep RTCPeerConnections alive for as long as the adapter is
    /// registered. Dropping the PC drops the DataChannel.
    pub(crate) webrtc_peer_connections:
        Mutex<HashMap<PeerId, std::sync::Arc<dyn webrtc::peer_connection::PeerConnection>>>,
    /// Global consumer callback for inbound WebRTC media tracks.
    pub(crate) webrtc_incoming_track_handler: Mutex<Option<IncomingTrackHandler>>,
    /// Runtime registry of inbound WebRTC media tracks by peer.
    pub(crate) webrtc_incoming_tracks: IncomingTrackRegistry,
    /// Live event fan-out. Every event the subscribers append to the
    /// store is also forwarded here so consumers tailing via
    /// [`Airc::subscribe`] see it immediately.
    pub(crate) live_tx: broadcast::Sender<Arc<TranscriptEvent>>,
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
    pub(crate) recently_broadcast: std::sync::Mutex<BroadcastDeduper>,
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

    /// Open the substrate for an explicit local agent name. This is
    /// the embeddable API behind `airc init --as <name>`; callers that
    /// do not pass a name can set `AIRC_AGENT_NAME` and use [`open`].
    pub async fn open_as(
        home: impl Into<PathBuf>,
        agent_name: impl Into<String>,
    ) -> Result<Self, AircError> {
        Self::open_with_policy_as(home, VerificationPolicy::Strict, agent_name).await
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

    /// Attach to an already-running daemon AND open the local identity
    /// under a specific `agent_name`. This is the constructor a multi-
    /// citizen host (Continuum personas, future external-agent hosts)
    /// reaches for when each hosted citizen needs:
    ///
    /// 1. Its OWN identity, distinguishable from other citizens'
    ///    (the [`open_as`] half), so `airc peers` from another scope
    ///    shows each one as a separate enrolled participant.
    /// 2. The daemon's live publish/subscribe (the [`attach`] half),
    ///    so the citizen can `say()` and `subscribe()` over the
    ///    router instead of only inspecting local state.
    ///
    /// Today, [`attach`] only offers (2) (under the host's default
    /// agent_name) and [`open_as`] only offers (1) (owner-mode, no
    /// daemon). Hosting multiple citizens in one process with the
    /// pre-existing pair therefore needs an unergonomic dance with
    /// the (intentionally private) `with_daemon_client` builder.
    /// This constructor closes the gap as a single call.
    ///
    /// Equivalent to:
    /// ```ignore
    /// let airc = Airc::open_as(home, agent_name).await?
    ///     .with_daemon_client(DaemonClient::new(socket));
    /// ```
    ///
    /// — but spelled as one method so consumers don't reach for the
    /// private builder.
    pub async fn attach_as(
        home: impl Into<PathBuf>,
        agent_name: impl Into<String>,
        socket: impl Into<PathBuf>,
    ) -> Result<Self, AircError> {
        let airc = Self::open_as(home, agent_name).await?;
        Ok(airc.with_daemon_client(DaemonClient::new(socket.into())))
    }

    /// Test-only [`attach`] that pins the machine-account wire root
    /// explicitly instead of deriving it from `HOME`/`USERPROFILE`.
    /// Two scopes sharing one `wire_root` resolve the same mesh
    /// identity (hence the same `RoomId`) and converge through the
    /// daemon — without mutating process-global env, which would race
    /// parallel tests. Strict verification, matching [`attach`].
    #[doc(hidden)]
    pub async fn attach_with_wire_root_for_test(
        home: impl Into<PathBuf>,
        wire_root: impl Into<PathBuf>,
        socket: impl Into<PathBuf>,
    ) -> Result<Self, AircError> {
        let airc = Self::open_with_wire_root_for_test(home, wire_root).await?;
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
        std::fs::create_dir_all(&home).map_err(IdentityError::Io)?;
        let wire_root = machine_account_home(&home);
        Self::open_inner(home, wire_root, policy, None).await
    }

    /// Variant of [`open_with_policy`] that pins the local agent name
    /// explicitly instead of reading `AIRC_AGENT_NAME`.
    pub async fn open_with_policy_as(
        home: impl Into<PathBuf>,
        policy: VerificationPolicy,
        agent_name: impl Into<String>,
    ) -> Result<Self, AircError> {
        let home: PathBuf = home.into();
        std::fs::create_dir_all(&home).map_err(IdentityError::Io)?;
        let wire_root = machine_account_home(&home);
        Self::open_inner(home, wire_root, policy, Some(agent_name.into())).await
    }

    /// Test-only: open with an explicit machine-account wire root rather
    /// than deriving it from `HOME`/`USERPROFILE` via
    /// `machine_account_home`. Lets in-process tests give each simulated
    /// "machine" its own isolated coordinator store + wire root WITHOUT
    /// mutating process-global env, which would race other parallel
    /// tests. Strict verification, matching `open`.
    #[doc(hidden)]
    pub async fn open_with_wire_root_for_test(
        home: impl Into<PathBuf>,
        wire_root: impl Into<PathBuf>,
    ) -> Result<Self, AircError> {
        Self::open_inner(
            home.into(),
            wire_root.into(),
            VerificationPolicy::Strict,
            None,
        )
        .await
    }

    async fn open_inner(
        home: PathBuf,
        wire_root: PathBuf,
        policy: VerificationPolicy,
        agent_name: Option<String>,
    ) -> Result<Self, AircError> {
        std::fs::create_dir_all(&home).map_err(IdentityError::Io)?;
        let identity = match agent_name {
            Some(agent_name) => LocalIdentity::load_or_generate_as(&home, agent_name).await?,
            None => LocalIdentity::load_or_generate(&home).await?,
        };
        std::fs::create_dir_all(&wire_root).map_err(IdentityError::Io)?;
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

        let registry = Arc::new(PeerKeyRegistry::new());
        registry
            .enrol(identity.peer_id, 0, identity.keypair.public_bytes())
            .map_err(|e| AircError::Crypto(e.to_string()))?;
        let mut enrolled = HashSet::from([identity.peer_id]);
        for stored in load_peer_registries(&home, &wire_root).await? {
            if !enrolled.insert(stored.peer_id) {
                continue;
            }
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
                route_health: Arc::new(TransportHealthTable::local_default()),
                route_endpoints: Arc::new(RouteEndpointTable::default()),
                imported_invites: Arc::new(ImportedInviteTable::default()),
                lamport_clock: AtomicU64::new(0),
                lan_tcp: Mutex::new(None),
                lan_subscriber: Mutex::new(None),
                relay: Mutex::new(None),
                relay_subscriber: Mutex::new(None),
                udp: Mutex::new(None),
                udp_subscriber: Mutex::new(None),
                webrtc_channels: Mutex::new(HashMap::new()),
                webrtc_subscribers: Mutex::new(HashMap::new()),
                webrtc_peer_connections: Mutex::new(HashMap::new()),
                webrtc_incoming_track_handler: Mutex::new(None),
                webrtc_incoming_tracks: IncomingTrackRegistry::default(),
                live_tx,
                recently_broadcast: std::sync::Mutex::new(BroadcastDeduper::with_capacity(
                    RECENTLY_BROADCAST_CAPACITY,
                )),
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
        ring.mark(event_id)
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

    /// Local agent-name discriminator for this identity row.
    pub fn agent_name(&self) -> &str {
        &self.inner.identity.agent_name
    }

    /// Persist a new local-identity card and broadcast the update to
    /// every currently-subscribed room so attached peers see the
    /// updated profile without rejoining (card da586598 — last
    /// identity-roster slice).
    ///
    /// Replaces the old direct-store save in the CLI's nick / identity
    /// edit path: storage + emission are now atomic from the caller's
    /// view (one fails, neither effect is visible to peers). Per-room
    /// emission iterates the SubscriptionSet's `subscribed` map.
    pub async fn set_local_identity_card(
        &self,
        identity: airc_core::identity::Identity,
    ) -> Result<(), AircError> {
        self.event_store()
            .save_local_identity_card(identity)
            .await
            .map_err(AircError::from)?;
        let set = subscriptions::load_or_init(self.event_store()).await?;
        for subscription in set.subscribed.values() {
            self.emit_peer_identity_card(subscription.room_id).await?;
        }
        Ok(())
    }

    /// Richer roster lookup — return the full `PeerIdentityCard` if
    /// `peer_id` has published one in the current room's recent
    /// window. Powers `airc whois <peer>` (card 20066c49) and any
    /// future caller that needs more than just the display name.
    /// Same scan window + on-demand model as [`Self::peer_alias`].
    pub async fn peer_identity_card(
        &self,
        peer_id: PeerId,
    ) -> Result<Option<airc_core::identity::PeerIdentityCard>, AircError> {
        let events = self.page_recent(200).await?;
        for event in events {
            if event.kind != airc_core::TranscriptKind::IdentityPublished {
                continue;
            }
            if event.peer_id != peer_id {
                continue;
            }
            let Some(airc_core::Body::Json(value)) = event.body else {
                continue;
            };
            let Ok(airc_core::identity::IdentityEvent::PeerIdentityCard(card)) =
                serde_json::from_value::<airc_core::identity::IdentityEvent>(value)
            else {
                continue;
            };
            return Ok(Some(card));
        }
        Ok(None)
    }

    /// Look up the latest doctrine published for the current room
    /// (card b898f713 — slice 3/4 of 2903a8ef). Same MVP shape as
    /// `Airc::peer_identity_card`: scan recent transcript events for
    /// the latest `TranscriptKind::DoctrinePublished`, decode the
    /// JSON body into `DoctrineEvent::RoomDoctrinePublished`, return
    /// it. Returns `Ok(None)` when the room has no published
    /// doctrine in the recent window (an honest "unknown" rather
    /// than rendering empty body).
    ///
    /// Consumer: slice 4/4 (card 745e93f0) — auto-load on attach so
    /// every newly-attaching agent has the operating contract in
    /// context without external onboarding.
    pub async fn room_doctrine(
        &self,
    ) -> Result<Option<airc_core::doctrine::RoomDoctrinePublished>, AircError> {
        let events = self.page_recent(200).await?;
        for event in events {
            if event.kind != airc_core::TranscriptKind::DoctrinePublished {
                continue;
            }
            let Some(airc_core::Body::Json(value)) = event.body else {
                continue;
            };
            let Ok(airc_core::doctrine::DoctrineEvent::RoomDoctrinePublished(card)) =
                serde_json::from_value::<airc_core::doctrine::DoctrineEvent>(value)
            else {
                continue;
            };
            return Ok(Some(card));
        }
        Ok(None)
    }

    /// Publish the room operating doctrine — card a9767579 (slice 2/4
    /// of engine-keystone 2903a8ef). Emits a
    /// `TranscriptKind::DoctrinePublished` lifecycle event on the
    /// CURRENT room carrying a serialized
    /// `DoctrineEvent::RoomDoctrinePublished` with the body and
    /// version. Every attaching agent's subscribe stream surfaces it;
    /// future slice 4/4 auto-loads it into agent context on join.
    ///
    /// `version` is a short content discriminator (e.g. first 12 chars
    /// of SHA-256 of `body`); caller chooses the function so older
    /// CLIs don't need a `sha2` dep to render `airc room doctrine
    /// publish --from-file AGENTS.md`. Idempotency is the publisher's
    /// responsibility — the substrate stores every event;
    /// projections take latest by `published_at_ms`.
    pub async fn publish_room_doctrine(
        &self,
        body: String,
        version: String,
    ) -> Result<(), AircError> {
        let room = self.current_room().await?;
        let event = airc_core::doctrine::DoctrineEvent::RoomDoctrinePublished(
            airc_core::doctrine::RoomDoctrinePublished {
                room_id: room.channel,
                body,
                version,
                published_by: self.inner.identity.peer_id,
                published_at_ms: time::now_ms()?,
            },
        );
        let body_json = serde_json::to_value(&event)
            .map_err(|e| AircError::Crypto(format!("doctrine event serialize: {e}")))?;
        self.emit_lifecycle(
            airc_core::TranscriptKind::DoctrinePublished,
            room.channel,
            airc_core::Body::Json(body_json),
        )
        .await
    }

    /// Card b4742d9c: pin a wall post in the current room.
    ///
    /// The wall is the room's living document — multiple typed posts
    /// (rules / agenda / principles / rag / consumer-defined) that any
    /// attaching agent or human can browse. Each post has a stable
    /// `post_id`; edits don't mutate the original, they emit a new
    /// post with `supersedes = Some(prior.post_id)` so the audit trail
    /// remains in the transcript.
    ///
    /// `category` is consumer-defined — common values include
    /// `"doctrine"`, `"rules"`, `"agenda"`, `"principles"`, `"rag"`,
    /// `"decision"`. The substrate has no opinion on the string;
    /// middleware (continuum routers, agent renderers, hermes /
    /// openclaw adapters) filter on it via header pattern.
    ///
    /// Returns the new post's `post_id` so the caller can later
    /// supersede it.
    pub async fn publish_wall_post(
        &self,
        category: String,
        body: String,
        supersedes: Option<uuid::Uuid>,
    ) -> Result<uuid::Uuid, AircError> {
        let room = self.current_room().await?;
        let post_id = uuid::Uuid::new_v4();
        let event = airc_core::doctrine::DoctrineEvent::WallPostPublished(
            airc_core::doctrine::WallPostPublished {
                room_id: room.channel,
                post_id,
                category,
                body,
                supersedes,
                published_by: self.inner.identity.peer_id,
                published_at_ms: crate::time::now_ms()?,
            },
        );
        let body_json = serde_json::to_value(&event)
            .map_err(|e| AircError::Crypto(format!("wall post serialize: {e}")))?;
        self.emit_lifecycle(
            airc_core::TranscriptKind::WallPostPublished,
            room.channel,
            airc_core::Body::Json(body_json),
        )
        .await?;
        Ok(post_id)
    }

    /// Card b4742d9c: fetch the current pinned wall posts for the
    /// current room, optionally filtered by `category`.
    ///
    /// Walks the recent transcript window, applies the supersede
    /// chain (a post is dropped from the result if a later post
    /// points to it via `supersedes`), and returns the surviving
    /// posts in published-time order. History (superseded versions)
    /// stays in the transcript — call `page_recent` directly to
    /// inspect it.
    ///
    /// Window size is generous (500) because a busy room may
    /// accumulate many pinned posts over time. If profiling shows
    /// this is too small for a long-lived room, the projection
    /// moves to a durable index — but for the substrate slice,
    /// scan-on-query is honest and avoids a cache that can drift.
    pub async fn wall_posts(
        &self,
        category_filter: Option<&str>,
    ) -> Result<Vec<airc_core::doctrine::WallPostPublished>, AircError> {
        let events = self.page_recent(500).await?;
        let mut posts = Vec::with_capacity(events.len());
        for event in events {
            if event.kind != airc_core::TranscriptKind::WallPostPublished {
                continue;
            }
            let Some(airc_core::Body::Json(value)) = event.body else {
                continue;
            };
            let Ok(airc_core::doctrine::DoctrineEvent::WallPostPublished(post)) =
                serde_json::from_value::<airc_core::doctrine::DoctrineEvent>(value)
            else {
                continue;
            };
            posts.push(post);
        }
        Ok(project_wall_posts(posts, category_filter))
    }

    /// **Card 1224aac2 slice 1.** Reserved wall-post category name for
    /// the room trust policy. Substrate consumes it (route gate);
    /// consumers publish under it.
    pub const WALL_CATEGORY_TRUST_POLICY: &'static str = "trust-policy";

    /// **Card 1224aac2 slice 1.** Return the active room trust policy
    /// declared via the current room's wall, or `None` when no
    /// `category="trust-policy"` post is present.
    ///
    /// A continuum recipe instantiates a room (settings room, private
    /// notes, team review, etc.) and declares its trust requirement by
    /// publishing a wall post:
    ///
    /// ```ignore
    /// airc.publish_wall_post("trust-policy",
    ///     serde_json::to_string(&RoomTrustPolicy {
    ///         min_tier: TrustTier::OwnAccount,
    ///     })?)?;
    /// ```
    ///
    /// The substrate then enforces it at the route policy: peers below
    /// `min_tier` (resolved per the existing trust gradient in
    /// `airc-trust`) are not delivered frames on this room — covering
    /// chat, widget commands, events, and presence equally.
    ///
    /// Latest-wins on the wall's supersede chain — a fresh trust-policy
    /// post supersedes the previous one. Missing or empty policy =
    /// no gate (back-compat with rooms predating this primitive).
    pub async fn wall_trust_policy_for_room(&self) -> Result<Option<RoomTrustPolicy>, AircError> {
        let posts = self
            .wall_posts(Some(Self::WALL_CATEGORY_TRUST_POLICY))
            .await?;
        // The wall projection returns surviving posts in
        // published-time order; the LATEST is the active policy.
        Ok(posts
            .last()
            .and_then(|post| serde_json::from_str(&post.body).ok()))
    }

    /// MVP identity-roster lookup (card e414817b, sub of 66d7e607).
    ///
    /// Scans recent transcript events in the current room for the
    /// latest `TranscriptKind::IdentityPublished` emitted by `peer_id`
    /// and returns the published display name when known. Returns
    /// `Ok(None)` when the peer has never published an identity card
    /// in this room's recent window, or when the published `name`
    /// field is empty (an honest "unknown" rather than rendering an
    /// empty string).
    ///
    /// On-demand query — no in-memory cache. The scan window (200
    /// events) is conservative for a substrate where `IdentityPublished`
    /// fires once per join, not per chat message. If profiling shows
    /// hot-path callers, the follow-up is an in-memory roster fed by
    /// the existing subscribe loop; `peer_alias` keeps its shape.
    ///
    /// Consumers: `airc work board format_peer` (card c397567a),
    /// `airc whois <peer>` (card 20066c49).
    pub async fn peer_alias(&self, peer_id: PeerId) -> Result<Option<String>, AircError> {
        let events = self.page_recent(200).await?;
        for event in events {
            if event.kind != airc_core::TranscriptKind::IdentityPublished {
                continue;
            }
            if event.peer_id != peer_id {
                continue;
            }
            let Some(airc_core::Body::Json(value)) = event.body else {
                continue;
            };
            let Ok(airc_core::identity::IdentityEvent::PeerIdentityCard(card)) =
                serde_json::from_value::<airc_core::identity::IdentityEvent>(value)
            else {
                continue;
            };
            if card.identity.name.is_empty() {
                return Ok(None);
            }
            return Ok(Some(card.identity.name));
        }
        Ok(None)
    }

    /// Sign a domain-separated identity assertion — the airc analogue
    /// of a WebAuthn assertion. The signature covers a versioned domain
    /// tag + `context` (the relying-party / "type" binding) + the
    /// `challenge` bytes (a server nonce, a session descriptor, or a
    /// Forge-alloy Merkle-context root), in a space disjoint from frame
    /// signatures. Consumers (Continuum / jtag / browser / server)
    /// build session tokens + credential bindings on top; the raw key
    /// is never exposed, so a later device-bound / Secure-Enclave
    /// signer (for hardware attestation) is a drop-in.
    pub fn sign_assertion(&self, context: &str, challenge: &[u8]) -> IdentityAssertion {
        self.inner.identity.keypair.sign_assertion(
            self.inner.identity.peer_id,
            0,
            context,
            challenge,
        )
    }

    pub fn is_daemon_attached(&self) -> bool {
        self.inner.daemon_client.is_some()
    }

    /// Register the callback invoked when any WebRTC peer connection
    /// negotiates an inbound media track.
    ///
    /// The callback is process-local runtime state. It is not
    /// persisted because `TrackRemote` handles are live transport
    /// objects, not replayable transcript data.
    pub async fn set_incoming_track_handler<F>(&self, handler: F) -> Result<(), AircError>
    where
        F: Fn(PeerId, IncomingTrack) + Send + Sync + 'static,
    {
        let mut guard = self.inner.webrtc_incoming_track_handler.lock().await;
        *guard = Some(std::sync::Arc::new(handler));
        Ok(())
    }

    /// Return the inbound WebRTC tracks currently registered for a
    /// peer. This is an inspection surface for consumers and tests;
    /// media samples themselves are read through the returned
    /// `TrackRemote` handles.
    pub async fn incoming_tracks_for_peer(&self, peer_id: PeerId) -> Vec<IncomingTrack> {
        self.inner
            .webrtc_incoming_tracks
            .tracks_for_peer(peer_id)
            .await
    }

    pub(crate) async fn handle_incoming_webrtc_track(&self, peer_id: PeerId, track: IncomingTrack) {
        self.inner
            .webrtc_incoming_tracks
            .record(peer_id, track.clone())
            .await;
        let handler = {
            let guard = self.inner.webrtc_incoming_track_handler.lock().await;
            guard.clone()
        };
        if let Some(handler) = handler {
            handler(peer_id, track);
        }
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
            route_health: Arc::new(TransportHealthTable::local_default()),
            route_endpoints: Arc::new(RouteEndpointTable::default()),
            imported_invites: Arc::new(ImportedInviteTable::default()),
            lamport_clock: AtomicU64::new(self.inner.lamport_clock.load(Ordering::Relaxed)),
            lan_tcp: Mutex::new(None),
            lan_subscriber: Mutex::new(None),
            relay: Mutex::new(None),
            relay_subscriber: Mutex::new(None),
            udp: Mutex::new(None),
            udp_subscriber: Mutex::new(None),
            webrtc_channels: Mutex::new(HashMap::new()),
            webrtc_subscribers: Mutex::new(HashMap::new()),
            webrtc_peer_connections: Mutex::new(HashMap::new()),
            webrtc_incoming_track_handler: Mutex::new(None),
            webrtc_incoming_tracks: IncomingTrackRegistry::default(),
            live_tx: self.inner.live_tx.clone(),
            recently_broadcast: std::sync::Mutex::new(BroadcastDeduper::with_capacity(
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
        self.inner.route_health.replace(samples);
        Ok(())
    }

    /// Snapshot the current route-health samples. Consumers can use
    /// this for diagnostics without reaching into resolver internals.
    pub fn transport_health(&self) -> Result<Vec<TransportHealthSample>, AircError> {
        Ok(self.inner.route_health.samples())
    }

    pub(crate) fn upsert_transport_health(
        &self,
        sample: TransportHealthSample,
    ) -> Result<(), AircError> {
        self.inner.route_health.upsert(sample);
        Ok(())
    }

    /// Resolve the mesh identity for this scope, going through the
    /// cache. Single-flighted at the file level: the cache only
    /// re-resolves after [`crate::mesh_identity::DEFAULT_TTL_MS`] so
    /// concurrent callers don't hammer `gh`. See the module docs for
    /// the resolver chain.
    ///
    /// Resolves against the machine-global **coordinator** store
    /// (`~/.airc/events.sqlite`), NOT the per-scope store. Per
    /// `docs/architecture/ACCOUNT-MESH-JOIN-CONTRACT.md`, "the account
    /// identity, not the machine [scope], is the room namespace." A
    /// per-scope cache let two scopes on the same machine resolve
    /// divergent identities (`gh_api_user` "joelteply" vs `git_email`
    /// "joelteply@yahoo.com"), and since the RoomId is derived from
    /// the identity, the same channel name fractured into two room_ids
    /// that silently could not see each other. The coordinator store
    /// is the machine-global convergence point the contract requires.
    ///
    /// If the coordinator has no cached identity yet but this scope's
    /// per-scope store does (older state, or a scope that resolved
    /// before the coordinator was seeded), adopt the per-scope value
    /// and promote it into the coordinator — keeping the coordinator
    /// authoritative while honoring an already-resolved identity. This
    /// avoids re-running the gh/git resolver and avoids fracturing
    /// rooms when the coordinator path differs from the scope path —
    /// e.g. on Windows, where `machine_account_home` collapses temp
    /// homes that live under `USERPROFILE` onto one coordinator store.
    pub(crate) async fn mesh_identity(&self) -> Result<MeshIdentity, AircError> {
        if mesh_identity::load_cached(self.coordinator_store())
            .await?
            .is_none()
        {
            if let Some(local) = mesh_identity::load_cached(self.event_store()).await? {
                mesh_identity::save(self.coordinator_store(), &local).await?;
            }
        }
        let cached = mesh_identity::resolve(self.coordinator_store()).await?;
        Ok(cached.as_mesh_identity())
    }

    pub(crate) async fn sync_account_peer_registry(&self) -> Result<(), AircError> {
        let peers = load_peer_registries(&self.inner.home, &self.inner.wire_root).await?;
        for peer in peers {
            self.inner
                .registry
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
        // Card c409eaf5: refuse uuid-shaped names. `ChannelName::new`
        // hashes the name into a derived UUID; a uuid-shaped string
        // re-hashes into a DIFFERENT channel UUID, silently. The
        // resulting subscription registers on the wrong channel and
        // the fan-out misses every publish. Better to fail loudly at
        // the API boundary than to let the trap close on the next
        // consumer like it closed on the continuum demo 2026-06-01.
        if uuid::Uuid::parse_str(name.trim()).is_ok() {
            return Err(AircError::JoinUuidString {
                string: name.to_string(),
            });
        }
        let channel = ChannelName::new(name)?;
        let identity = self.mesh_identity().await?;
        let mut set = subscriptions::load_or_init(self.event_store()).await?;
        let subscription =
            set.subscribe_with_wire_root(&self.inner.wire_root, &identity, channel.clone())?;
        set.set_default(channel.clone())?;
        subscriptions::save(self.event_store(), &set).await?;
        self.publish_presence(&identity, &set).await?;
        let room = subscription.as_room();

        // Emit the lifecycle event after the subscription is durable
        // and the wire is up. Lifecycle is part of the substrate
        // contract: if the durable state cannot be published into the
        // event stream, fail loudly instead of hiding an unobservable
        // state transition.
        let body_json = serde_json::to_value(crate::lifecycle::RoomJoinedBody {
            channel_name: channel.as_str().to_string(),
            room_id: room.channel,
            wire: room.wire.display().to_string(),
            is_default: true,
        })
        .map_err(|e| AircError::Crypto(format!("lifecycle body serialize: {e}")))?;
        let body = airc_core::Body::Json(body_json);
        self.emit_lifecycle(airc_core::TranscriptKind::RoomJoined, room.channel, body)
            .await?;

        // Publish this peer's identity card to the new room so peers
        // already attached populate their roster on the next event
        // they receive (card 2f74b8a1 — identity-roster substrate
        // slice, parent af40f46d). Re-loaded from the store on each
        // join so any local-identity edits since startup propagate.
        self.emit_peer_identity_card(room.channel).await?;

        Ok(room)
    }

    /// Build + emit a `PeerIdentityCard` for this scope on the given
    /// room as an `IdentityPublished` lifecycle event. Used on join
    /// (so the room's roster sees this peer arrive identifiable, not
    /// just by uuid); will also be used on nick / profile change in a
    /// follow-up slice. No-op when no local identity is persisted
    /// yet (e.g. fresh scope mid-bootstrap).
    async fn emit_peer_identity_card(&self, room_id: airc_core::RoomId) -> Result<(), AircError> {
        let local = self
            .event_store()
            .load_local_identity()
            .await
            .map_err(AircError::from)?;
        let Some(stored) = local else { return Ok(()) };
        let card = airc_core::identity::PeerIdentityCard {
            peer_id: self.inner.identity.peer_id,
            identity: stored.identity,
            emitted_at_ms: time::now_ms()?,
        };
        let event = airc_core::identity::IdentityEvent::PeerIdentityCard(card);
        let body_json = serde_json::to_value(&event)
            .map_err(|e| AircError::Crypto(format!("identity event serialize: {e}")))?;
        let body = airc_core::Body::Json(body_json);
        self.emit_lifecycle(airc_core::TranscriptKind::IdentityPublished, room_id, body)
            .await
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

        // Card a6d0df25: publish this peer's identity card to every
        // room subscribed in this attach so peers already attached
        // populate their roster — same lifecycle as Airc::join(name)
        // (commit 088af06), now extended to the routine `airc join`
        // (no args) path. No-op when no local identity exists.
        for room in &rooms {
            self.emit_peer_identity_card(room.channel).await?;
        }

        Ok(rooms)
    }

    /// Leave a subscribed channel without deleting identity or trust.
    ///
    /// `None` parts the current default channel. The removed channel is
    /// tombstoned in the subscription set so a later
    /// [`join_default_context`](Self::join_default_context) does not
    /// silently re-add a channel the caller explicitly left.
    pub async fn part_channel(&self, name: Option<&str>) -> Result<Room, AircError> {
        let identity = self.mesh_identity().await?;
        let mut set = subscriptions::load_or_init(self.event_store()).await?;
        let channel = match name {
            Some(name) => ChannelName::new(name)?,
            None => set.default.clone().ok_or(AircError::NoCurrentRoom)?,
        };
        let removed = set
            .unsubscribe(&channel)
            .ok_or_else(|| AircError::NotSubscribed(channel.display_with_hash()))?;
        subscriptions::save(self.event_store(), &set).await?;
        self.publish_presence(&identity, &set).await?;

        let room = removed.as_room();
        let body_json = serde_json::to_value(crate::lifecycle::RoomPartedBody {
            channel_name: channel.as_str().to_string(),
            room_id: room.channel,
        })
        .map_err(|e| AircError::Crypto(format!("lifecycle body serialize: {e}")))?;
        self.emit_lifecycle(
            airc_core::TranscriptKind::RoomParted,
            room.channel,
            airc_core::Body::Json(body_json),
        )
        .await?;

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
        // Same publish-on-subscribe semantics as Airc::join /
        // ensure_join_context (card a6d0df25): the lazy default-room
        // subscribe is a real attach point, so emit the identity
        // card to the new room.
        let room = subscription.as_room();
        self.emit_peer_identity_card(room.channel).await?;
        Ok(room)
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
        let room = self.current_room().await?;
        self.inner
            .store
            .save_runtime_cursor(consumer_id, cursor, time::now_ms()?)
            .await?;
        self.emit_subscription_advanced(consumer_id, cursor, room.channel)
            .await?;
        Ok(())
    }

    /// Persist a runtime consumer checkpoint for a concrete event.
    ///
    /// This is the preferred path for hooks and live feeds because it
    /// carries the source event's room and kind. Cursor advancement for
    /// a `SubscriptionAdvanced` lifecycle event is stored but does not
    /// emit another `SubscriptionAdvanced`, preventing self-amplifying
    /// cursor loops.
    pub async fn save_runtime_cursor_for_event(
        &self,
        consumer_id: &str,
        event: &airc_core::TranscriptEvent,
    ) -> Result<(), AircError> {
        let cursor = event.cursor();
        self.inner
            .store
            .save_runtime_cursor(consumer_id, &cursor, time::now_ms()?)
            .await?;
        if event.kind != airc_core::TranscriptKind::SubscriptionAdvanced {
            self.emit_subscription_advanced(consumer_id, &cursor, event.room_id)
                .await?;
        }
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

/// Card b4742d9c — pure projection: walk a set of WallPostPublished
/// events (in transcript order) and return the currently-pinned set,
/// applying supersede chains.
///
/// The substrate guarantees the transcript replay order is stable;
/// this function takes that as input and applies the supersede
/// semantics: if a later post points at an earlier `post_id` via
/// `supersedes`, the earlier post is no longer "pinned" — only the
/// latest in each chain survives.
///
/// `category_filter` is applied BEFORE supersede resolution. This
/// matters: a `Some("doctrine")` filter shows only doctrine posts
/// and their doctrine-category supersedes; doctrine posts don't
/// supersede `"rules"` posts even if they share a post_id by
/// accident (post_ids are UUIDv4, so accidental sharing is
/// astronomically unlikely, but the semantics MUST be category-
/// scoped regardless).
///
/// **Card 1224aac2 slice 1.** A room's trust policy, published as
/// the body of a `WallPostPublished` event in
/// `category=Airc::WALL_CATEGORY_TRUST_POLICY`.
///
/// `min_tier` is the minimum trust tier a peer must hold (resolved
/// per `airc_trust::resolve_tier`) to receive frames on the room.
/// The substrate enforces it at the route boundary — chat, widget
/// commands, events, and presence are gated equally so a single
/// post governs the whole UX surface for the room.
///
/// Adding fields to this struct is backward-compatible as long as
/// they are `#[serde(default)]` — older posts written without them
/// decode with the default, and renderers that don't know about a
/// new field continue to work.
///
/// `TrustTier` itself doesn't ship serde derives (the canonical wire
/// form is the `as_wire_str` / `from_wire_str` schema-bound pair).
/// We round-trip via those so a string change there is caught by the
/// existing variant round-trip test rather than silently shifting
/// our JSON shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomTrustPolicy {
    /// Minimum trust tier required to receive frames on this room.
    /// Resolved against each peer's `airc_trust::resolve_tier` outcome
    /// at delivery time; peers below this tier are dropped at the
    /// router boundary (chat, widget commands, events, presence —
    /// every frame the room would have delivered).
    #[serde(with = "trust_tier_wire_str")]
    pub min_tier: TrustTier,
}

/// Serde adapter that round-trips [`TrustTier`] via its stable
/// `as_wire_str` / `from_wire_str` schema-bound representation.
/// Avoids deriving serde directly on `TrustTier` (which would create
/// two competing wire forms — the snake_case enum-discriminant default
/// and the existing stable schema).
mod trust_tier_wire_str {
    use super::TrustTier;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &TrustTier, s: S) -> Result<S::Ok, S::Error> {
        value.as_wire_str().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TrustTier, D::Error> {
        let raw = String::deserialize(d)?;
        TrustTier::from_wire_str(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown trust tier wire-string {raw:?} — \
                 valid values are own_machine, own_account, friend, untrusted"
            ))
        })
    }
}

/// Pure, sync, no IO — the IO half is `Airc::wall_posts`, which
/// reads the transcript and hands the result here.
fn project_wall_posts(
    mut events: Vec<airc_core::doctrine::WallPostPublished>,
    category_filter: Option<&str>,
) -> Vec<airc_core::doctrine::WallPostPublished> {
    if let Some(cat) = category_filter {
        events.retain(|p| p.category == cat);
    }
    // Two-pass: first collect the set of post_ids that have been
    // superseded by ANY later post in the SAME CATEGORY. Then return
    // only the posts that aren't in that set.
    //
    // Same-category gate: a `"rules"` post's supersedes pointing at
    // a `"doctrine"` post_id is treated as a no-op (consumer bug),
    // not as a cross-category supersede. The substrate doesn't let
    // categories interfere with each other.
    let mut superseded: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
    // Group by category for the integrity gate.
    let mut category_of: std::collections::HashMap<uuid::Uuid, String> =
        std::collections::HashMap::new();
    for p in &events {
        category_of.insert(p.post_id, p.category.clone());
    }
    for p in &events {
        if let Some(parent) = p.supersedes {
            if category_of.get(&parent) == Some(&p.category) {
                superseded.insert(parent);
            }
        }
    }
    let mut result: Vec<_> = events
        .into_iter()
        .filter(|p| !superseded.contains(&p.post_id))
        .collect();
    // Stable order by published_at_ms so a consumer rendering the
    // wall sees posts in pin-time order regardless of how the
    // transcript scan happened to traverse them.
    result.sort_by_key(|p| p.published_at_ms);
    result
}

#[cfg(test)]
mod wall_projection_tests {
    use super::project_wall_posts;
    use airc_core::doctrine::WallPostPublished;
    use airc_core::{PeerId, RoomId};
    use uuid::Uuid;

    fn post(
        post_id: u128,
        category: &str,
        body: &str,
        supersedes: Option<u128>,
        published_at_ms: u64,
    ) -> WallPostPublished {
        WallPostPublished {
            room_id: RoomId::from_u128(1),
            post_id: Uuid::from_u128(post_id),
            category: category.to_string(),
            body: body.to_string(),
            supersedes: supersedes.map(Uuid::from_u128),
            published_by: PeerId::from_u128(99),
            published_at_ms,
        }
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = project_wall_posts(Vec::new(), None);
        assert!(result.is_empty());
    }

    #[test]
    fn single_post_with_no_supersedes_survives() {
        let events = vec![post(1, "rules", "use rust-rewrite", None, 100)];
        let result = project_wall_posts(events.clone(), None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].body, "use rust-rewrite");
    }

    #[test]
    fn revision_supersedes_original_within_same_category() {
        // The living-document core case: edit a "rules" post and
        // the original drops off the wall.
        let events = vec![
            post(1, "rules", "original", None, 100),
            post(2, "rules", "revised", Some(1), 200),
        ];
        let result = project_wall_posts(events, None);
        assert_eq!(result.len(), 1, "only the latest revision survives");
        assert_eq!(result[0].body, "revised");
    }

    #[test]
    fn supersede_chain_of_three_yields_only_the_latest() {
        // A series of revisions: v1 → v2 → v3. The wall shows v3
        // only; v1 and v2 are history (still in the transcript,
        // not in the result).
        let events = vec![
            post(1, "rules", "v1", None, 100),
            post(2, "rules", "v2", Some(1), 200),
            post(3, "rules", "v3", Some(2), 300),
        ];
        let result = project_wall_posts(events, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].body, "v3");
    }

    #[test]
    fn cross_category_supersede_is_a_noop_substrate_does_not_let_categories_interfere() {
        // A "rules" post whose supersedes points at a "doctrine"
        // post_id is a CONSUMER bug. The substrate must NOT honor
        // it — categories are independent walls sharing the same
        // event-kind discriminator.
        let events = vec![
            post(1, "doctrine", "auto-loaded on attach", None, 100),
            post(
                2,
                "rules",
                "this rules post tries to supersede the doctrine",
                Some(1),
                200,
            ),
        ];
        let result = project_wall_posts(events, None);
        // BOTH posts survive: the cross-category supersede was a no-op.
        assert_eq!(result.len(), 2);
        // The doctrine post is still pinned despite the rules-post
        // claiming to supersede it.
        assert!(result.iter().any(|p| p.body == "auto-loaded on attach"));
        assert!(result.iter().any(|p| p.body.contains("rules post tries")));
    }

    #[test]
    fn category_filter_returns_only_matching_posts() {
        let events = vec![
            post(1, "doctrine", "agent ops", None, 100),
            post(2, "rules", "use rust-rewrite", None, 200),
            post(3, "agenda", "ship cell-grid", None, 300),
        ];
        let result = project_wall_posts(events.clone(), Some("rules"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].body, "use rust-rewrite");
    }

    #[test]
    fn category_filter_applied_before_supersede_so_chains_remain_intact() {
        // The filter must NOT make a supersede chain disappear by
        // hiding the parent: if the filter is "rules", both rules
        // posts are visible to the projection, supersede is applied
        // within rules, and only the latest survives.
        let events = vec![
            post(1, "rules", "v1", None, 100),
            post(2, "doctrine", "unrelated", None, 150),
            post(3, "rules", "v2", Some(1), 200),
        ];
        let result = project_wall_posts(events, Some("rules"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].body, "v2");
    }

    #[test]
    fn results_ordered_by_published_at_ms_ascending() {
        // Wall renderers display posts in pin-time order so the
        // room's evolution reads naturally top-to-bottom. Pin this
        // order so a consumer can always count on it.
        let events = vec![
            post(3, "rules", "latest", None, 300),
            post(1, "rules", "earliest", None, 100),
            post(2, "rules", "middle", None, 200),
        ];
        let result = project_wall_posts(events, None);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].published_at_ms, 100);
        assert_eq!(result[1].published_at_ms, 200);
        assert_eq!(result[2].published_at_ms, 300);
    }

    #[test]
    fn consumer_defined_category_strings_are_carried_verbatim() {
        // The substrate doesn't normalize / canonicalize category
        // strings. hermes can use `"plan-step"`, continuum can use
        // `"capability-ad"`, openclaw can use `"tool-permission"`,
        // a friend's adapter can use whatever — and filtering on
        // the exact string works without coordination.
        let events = vec![
            post(1, "hermes:plan-step", "extract intent", None, 100),
            post(2, "continuum:capability-ad", "gpu 5090 24gb", None, 200),
            post(3, "joel:reminders", "ship the wall card", None, 300),
        ];
        let hermes = project_wall_posts(events.clone(), Some("hermes:plan-step"));
        let continuum = project_wall_posts(events.clone(), Some("continuum:capability-ad"));
        let reminders = project_wall_posts(events, Some("joel:reminders"));
        assert_eq!(hermes.len(), 1);
        assert_eq!(continuum.len(), 1);
        assert_eq!(reminders.len(), 1);
    }

    #[test]
    fn supersedes_pointing_at_unknown_post_id_is_a_noop() {
        // A post whose supersedes references a post_id we've never
        // seen (window-truncation, replay-from-future) is treated
        // as a fresh pin in the visible window. Conservative: the
        // unknown parent isn't in our view, so we can't honor the
        // supersede; we just keep the new post.
        let events = vec![post(1, "rules", "supersedes a phantom", Some(999_999), 100)];
        let result = project_wall_posts(events, None);
        assert_eq!(result.len(), 1, "post survives despite unknown parent");
    }
}

#[cfg(test)]
mod room_trust_policy_tests {
    use super::{RoomTrustPolicy, TrustTier};

    /// Card 1224aac2 slice 1: the wire shape MUST use the stable
    /// `as_wire_str` schema from `airc_store::peer_trust`, not the
    /// debug / variant-discriminant default that serde would pick on
    /// its own. A schema drift here would silently change every
    /// existing room's policy interpretation, so pin the wire form
    /// against representative variants.
    #[test]
    fn round_trips_via_stable_trust_tier_wire_strings() {
        for tier in TrustTier::ALL_VARIANTS {
            let policy = RoomTrustPolicy { min_tier: *tier };
            let json = serde_json::to_string(&policy).expect("serialize");
            let decoded: RoomTrustPolicy = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(policy, decoded, "round-trip for tier {tier:?}");
            // Pin the on-wire field name + value so an accidental
            // rename (`min_tier` → `minTier`, or a field projection
            // change) is caught at the wire layer not at integration
            // time.
            assert!(
                json.contains("\"min_tier\""),
                "JSON must carry field name `min_tier`: {json}"
            );
            assert!(
                json.contains(&format!("\"{}\"", tier.as_wire_str())),
                "JSON must carry stable wire-str {:?}: {json}",
                tier.as_wire_str()
            );
        }
    }

    /// Card 1224aac2 slice 1: a body string with an unknown trust
    /// tier value MUST fail to decode with a useful error — not
    /// silently fall back to a default. Substrate is fail-loud on
    /// schema drift; route enforcement upstream relies on this.
    #[test]
    fn unknown_tier_string_surfaces_decode_error() {
        let body = r#"{"min_tier":"ultra_secret"}"#;
        let result: Result<RoomTrustPolicy, _> = serde_json::from_str(body);
        let err = result.expect_err("unknown tier must error");
        let message = err.to_string();
        assert!(
            message.contains("ultra_secret"),
            "error must surface the bad value: {message}"
        );
    }

    /// Card 1224aac2 slice 1: the reserved wall-category constant
    /// stays stable as the contract between recipe consumers
    /// publishing policies and the substrate enforcing them. A
    /// silent rename here would leave every existing policy post
    /// unrecognized by the next binary upgrade.
    #[test]
    fn wall_category_constant_is_stable() {
        assert_eq!(super::Airc::WALL_CATEGORY_TRUST_POLICY, "trust-policy");
    }

    /// Card b0a81c31: a temp-rooted scope must stay its own account
    /// boundary on every platform. On Windows `%TEMP%` lives inside
    /// `%USERPROFILE%`, so before the temp_dir guard this resolved to
    /// the REAL machine account (`$USERPROFILE/.airc`) and leaked real
    /// peer registries / daemon state into hermetic tests — failing
    /// only on machines with actual enrolled peers, never in CI.
    #[test]
    fn machine_account_home_keeps_temp_scopes_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let scope = dir.path().join("scope-home");
        std::fs::create_dir(&scope).unwrap();
        assert_eq!(super::machine_account_home(&scope), scope);
    }
}
