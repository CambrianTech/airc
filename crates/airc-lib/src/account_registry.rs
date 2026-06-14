//! Account-mesh remote registry boundary.
//!
//! The machine-global coordinator owns local presence under
//! `<machine-home>/.airc/accounts/<mesh-identity>/`. This module is the
//! remote synchronization contract above that local truth: serialize a
//! signed/trusted set of peer beacons + route metadata, publish it to a
//! registry adapter, and import it on another machine.
//!
//! GitHub/gists are one possible adapter for this trait, but they carry
//! only this registry document. Runtime messages, transcript events,
//! media, and model payloads are explicitly out of scope.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use airc_store::{SqliteEventStore, StoredAccountRegistry};

use crate::coordinator::{CoordinatorSnapshot, PresenceBeacon};
use crate::error::AircError;
use crate::registry::PeerSpec;
use crate::route::{InviteBeacon, RouteEndpoint};
use crate::subscriptions::{ChannelName, MeshIdentity};
use crate::time;
use crate::Airc;

pub const ACCOUNT_REGISTRY_SCHEMA_VERSION: u16 = 1;

/// Temp-rooted scope-home detection (#1150). The definition moved to
/// [`airc_core::temp_home`] (card f122b5b5) so `airc-daemon`'s idle
/// self-exit watchdog consults the SAME check without depending on
/// this crate; re-exported here so existing callers keep their
/// import path.
pub use airc_core::scope_home_is_temp_rooted;

/// Outcome of [`merge_registry_documents`]: the merged view plus the
/// hygiene counters the caller must surface (count, not full dump).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryMergeOutcome {
    pub document: Option<AccountRegistryDocument>,
    /// Beacons dropped because their `scope_home` is temp-rooted —
    /// phantom hermetic-test peers from old polluted documents
    /// (card d793c242). Never enrolled, never dialed.
    pub ignored_temp_beacons: usize,
}

/// Reader-side merge across per-machine registry documents.
///
/// The rendezvous is one document per WRITER (machine); a reader must
/// combine them. Previously the reader picked the single newest
/// document wholesale, which (a) dropped beacons only present in older
/// machines' documents and (b) trusted whatever a polluted document
/// carried. This merge is intentional:
///
/// - documents that fail validation or belong to a different mesh
///   identity are skipped;
/// - beacons whose `scope_home` is temp-rooted are IGNORED and counted
///   ([`scope_home_is_temp_rooted`]) — belt-and-braces against the
///   already-published phantom test peers of card d793c242;
/// - per `peer_id`, the FRESHEST beacon wins (highest
///   `heartbeat_at_ms`, tie-broken by the carrying document's
///   `generated_at_ms`);
/// - if the freshest beacon for a peer carries NO endpoints but an
///   older beacon for the same peer does, the merged beacon keeps the
///   freshest presence and retains the freshest NON-EMPTY endpoint
///   set (card 4b6a0ffa / #33: an endpoint-less manual-sync document
///   must not shadow a dialable daemon document — same doctrine as
///   the import guard "empty beacons leave the column alone");
/// - channels are the sorted union; `generated_at_ms` is the max of
///   the contributing documents.
pub fn merge_registry_documents(
    documents: Vec<AccountRegistryDocument>,
    mesh_identity: &MeshIdentity,
) -> RegistryMergeOutcome {
    let mut ignored_temp_beacons = 0usize;
    let mut generated_at_ms = 0u64;
    let mut channels: Vec<ChannelName> = Vec::new();
    // peer_id -> (heartbeat_at_ms, doc generated_at_ms, beacon)
    let mut freshest: HashMap<airc_core::PeerId, (u64, u64, AccountPeerBeacon)> = HashMap::new();
    // peer_id -> (heartbeat_at_ms, doc generated_at_ms, endpoints) of
    // the freshest beacon that actually CARRIES endpoints — the
    // backfill source when the overall-freshest beacon is endpoint-less.
    type EndpointKey = (u64, u64, Vec<RouteEndpoint>);
    let mut endpoint_carriers: HashMap<airc_core::PeerId, EndpointKey> = HashMap::new();
    let mut matched_any = false;

    for document in documents {
        if document.mesh_identity != *mesh_identity || document.validate().is_err() {
            continue;
        }
        matched_any = true;
        generated_at_ms = generated_at_ms.max(document.generated_at_ms);
        for channel in &document.channels {
            if !channels.contains(channel) {
                channels.push(channel.clone());
            }
        }
        for beacon in document.peers {
            if scope_home_is_temp_rooted(&beacon.presence.scope_home) {
                ignored_temp_beacons += 1;
                continue;
            }
            let key = (beacon.presence.heartbeat_at_ms, document.generated_at_ms);
            if !beacon.endpoints.is_empty() {
                match endpoint_carriers.get(&beacon.peer_id()) {
                    Some((heartbeat, doc_ms, _)) if (*heartbeat, *doc_ms) >= key => {}
                    _ => {
                        endpoint_carriers
                            .insert(beacon.peer_id(), (key.0, key.1, beacon.endpoints.clone()));
                    }
                }
            }
            match freshest.get(&beacon.peer_id()) {
                Some((heartbeat, doc_ms, _)) if (*heartbeat, *doc_ms) >= key => {}
                _ => {
                    freshest.insert(beacon.peer_id(), (key.0, key.1, beacon));
                }
            }
        }
    }

    if !matched_any {
        return RegistryMergeOutcome {
            document: None,
            ignored_temp_beacons,
        };
    }

    let mut peers: Vec<AccountPeerBeacon> = freshest
        .into_values()
        .map(|(_, _, beacon)| beacon)
        .collect();
    // Endpoint retention (card 4b6a0ffa / #33): an endpoint-less winner
    // (e.g. a fresher manual-sync doc) must not erase a peer's known
    // dialable endpoints from the merged view.
    for peer in &mut peers {
        if peer.endpoints.is_empty() {
            if let Some((_, _, endpoints)) = endpoint_carriers.remove(&peer.peer_id()) {
                peer.endpoints = endpoints;
            }
        }
    }
    peers.sort_by_key(|peer| peer.peer_id().to_string());
    channels.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    RegistryMergeOutcome {
        document: Some(AccountRegistryDocument::new(
            mesh_identity.clone(),
            generated_at_ms,
            channels,
            peers,
        )),
        ignored_temp_beacons,
    }
}

/// Default freshness horizon for reader-side beacon pruning: 10 minutes.
///
/// Deliberately MUCH longer than the local coordinator's 60s heartbeat
/// TTL ([`crate::coordinator::DEFAULT_HEARTBEAT_TTL_MS`]). A peer's
/// beacon reaches another machine only after a publish→gist→fetch hop,
/// and the production registry-refresh loop runs on a ~120s cadence, so
/// a perfectly live peer's freshest visible beacon is routinely a couple
/// of minutes old purely from transport latency. The local TTL would
/// false-drop those. This horizon is sized to prune only beacons whose
/// publisher is genuinely gone (process dead for many minutes / the gist
/// is days stale) while never evicting a live-but-laggy peer.
pub const DEFAULT_PEER_FRESHNESS_TTL_MS: u64 = 600_000;

/// Drop peers whose freshest beacon is staler than `ttl_ms` relative to
/// `now_ms`, in place. Returns the number pruned.
///
/// This is the reader-side counterpart to the local coordinator's
/// [`crate::coordinator::drain_stale_store`]: [`merge_registry_documents`]
/// already keeps only the FRESHEST beacon per peer, but a peer whose
/// freshest beacon is itself ancient (the publisher died and its gist
/// never got cleaned) would still be enrolled and then dialed — the
/// stale-route orphan path. Pruning here means the enrol set never
/// contains a route we already know is dead.
///
/// Reuses [`PresenceBeacon::is_fresh`] so the freshness decision lives in
/// exactly one place (saturating-sub guards future-dated clocks → a
/// future-dated beacon is treated as fresh, never pruned).
pub fn prune_stale_peers(peers: &mut Vec<AccountPeerBeacon>, now_ms: u64, ttl_ms: u64) -> usize {
    let before = peers.len();
    peers.retain(|peer| peer.presence.is_fresh(now_ms, ttl_ms));
    before - peers.len()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRegistryDocument {
    pub schema_version: u16,
    pub mesh_identity: MeshIdentity,
    pub generated_at_ms: u64,
    pub channels: Vec<ChannelName>,
    pub peers: Vec<AccountPeerBeacon>,
}

impl AccountRegistryDocument {
    pub fn new(
        mesh_identity: MeshIdentity,
        generated_at_ms: u64,
        channels: Vec<ChannelName>,
        peers: Vec<AccountPeerBeacon>,
    ) -> Self {
        Self {
            schema_version: ACCOUNT_REGISTRY_SCHEMA_VERSION,
            mesh_identity,
            generated_at_ms,
            channels,
            peers,
        }
    }

    pub fn from_snapshot(
        snapshot: &CoordinatorSnapshot,
        peer_specs: impl IntoIterator<Item = PeerSpec>,
        endpoints: impl IntoIterator<Item = (airc_core::PeerId, Vec<RouteEndpoint>)>,
        generated_at_ms: u64,
    ) -> Self {
        let specs: HashMap<_, _> = peer_specs
            .into_iter()
            .map(|spec| (spec.peer_id, spec))
            .collect();
        let endpoints: HashMap<_, _> = endpoints.into_iter().collect();
        let mut peers: Vec<_> = snapshot
            .live
            .iter()
            .filter_map(|presence| {
                let peer_spec = specs.get(&presence.peer_id)?.clone();
                Some(AccountPeerBeacon {
                    presence: presence.clone(),
                    peer_spec,
                    endpoints: endpoints
                        .get(&presence.peer_id)
                        .cloned()
                        .unwrap_or_default(),
                })
            })
            .collect();
        peers.sort_by_key(|peer| peer.peer_id().to_string());

        Self::new(
            snapshot.mesh_identity.clone(),
            generated_at_ms,
            snapshot.live_channels.clone(),
            peers,
        )
    }

    pub fn validate(&self) -> Result<(), AccountRegistryError> {
        if self.schema_version != ACCOUNT_REGISTRY_SCHEMA_VERSION {
            return Err(AccountRegistryError::SchemaVersionMismatch {
                found: self.schema_version,
                expected: ACCOUNT_REGISTRY_SCHEMA_VERSION,
            });
        }
        for peer in &self.peers {
            if peer.presence.peer_id != peer.peer_spec.peer_id {
                return Err(AccountRegistryError::PeerMismatch {
                    presence_peer_id: peer.presence.peer_id,
                    spec_peer_id: peer.peer_spec.peer_id,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPeerBeacon {
    pub presence: PresenceBeacon,
    pub peer_spec: PeerSpec,
    pub endpoints: Vec<RouteEndpoint>,
}

impl AccountPeerBeacon {
    pub fn peer_id(&self) -> airc_core::PeerId {
        self.peer_spec.peer_id
    }

    pub fn invite_beacon(&self) -> InviteBeacon {
        InviteBeacon::new(
            self.peer_spec.peer_id,
            self.peer_spec.clone(),
            self.endpoints.clone(),
        )
    }
}

#[derive(Debug)]
pub enum AccountRegistryError {
    SchemaVersionMismatch {
        found: u16,
        expected: u16,
    },
    PeerMismatch {
        presence_peer_id: airc_core::PeerId,
        spec_peer_id: airc_core::PeerId,
    },
    Adapter(String),
}

impl std::fmt::Display for AccountRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersionMismatch { found, expected } => {
                write!(
                    f,
                    "account registry schema version {found}, expected {expected}"
                )
            }
            Self::PeerMismatch {
                presence_peer_id,
                spec_peer_id,
            } => write!(
                f,
                "account registry peer mismatch: presence {presence_peer_id} vs spec {spec_peer_id}"
            ),
            Self::Adapter(error) => write!(f, "account registry adapter: {error}"),
        }
    }
}

impl std::error::Error for AccountRegistryError {}

#[async_trait]
pub trait AccountRegistryStore: Send + Sync {
    async fn publish(&self, document: &AccountRegistryDocument)
        -> Result<(), AccountRegistryError>;

    async fn refresh(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<Option<AccountRegistryDocument>, AccountRegistryError>;
}

/// Store-backed local cache of account-registry documents.
///
/// Replaces the previous on-disk `<root>/<mesh-identity>/registry.json`
/// sidecar with a row in the `account_registry` SeaORM table. Pairs
/// well with remote adapters (e.g. `GhAccountRegistryStore`) — those
/// publish to a remote rendezvous and use this store as the local
/// cache of "what we last sent/received."
#[derive(Clone)]
pub struct SqliteAccountRegistryStore {
    store: Arc<SqliteEventStore>,
}

impl SqliteAccountRegistryStore {
    pub fn new(store: Arc<SqliteEventStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AccountRegistryStore for SqliteAccountRegistryStore {
    async fn publish(
        &self,
        document: &AccountRegistryDocument,
    ) -> Result<(), AccountRegistryError> {
        document.validate()?;
        let document_json = serde_json::to_string(document).map_err(|error| {
            AccountRegistryError::Adapter(format!("serialize registry document: {error}"))
        })?;
        let now_ms = time::now_ms().map_err(|error| {
            AccountRegistryError::Adapter(format!("clock for registry save: {error}"))
        })?;
        self.store
            .save_account_registry(StoredAccountRegistry {
                mesh_identity: document.mesh_identity.as_str().to_string(),
                schema_version: document.schema_version,
                generated_at_ms: document.generated_at_ms,
                document_json,
                updated_at_ms: now_ms,
            })
            .await
            .map_err(|error| {
                AccountRegistryError::Adapter(format!("persist registry document: {error}"))
            })
    }

    async fn refresh(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<Option<AccountRegistryDocument>, AccountRegistryError> {
        let row = self
            .store
            .load_account_registry(mesh_identity.as_str())
            .await
            .map_err(|error| {
                AccountRegistryError::Adapter(format!("load registry document: {error}"))
            })?;
        let Some(stored) = row else {
            return Ok(None);
        };
        let document: AccountRegistryDocument = serde_json::from_str(&stored.document_json)
            .map_err(|error| {
                AccountRegistryError::Adapter(format!("parse registry document: {error}"))
            })?;
        document.validate()?;
        Ok(Some(document))
    }
}

impl Airc {
    pub async fn account_registry_document(&self) -> Result<AccountRegistryDocument, AircError> {
        let identity = self.mesh_identity().await?;
        let snapshot = crate::coordinator::snapshot_store(
            self.coordinator_store(),
            &identity,
            &crate::coordinator::CoordinatorConfig::default(),
            crate::time::now_ms()?,
        )
        .await?;
        let self_id = self.inner.identity.peer_id;
        let self_spec = PeerSpec {
            peer_id: self_id,
            pubkey: self.inner.identity.keypair.public_bytes(),
        };
        let mut peer_specs = vec![self_spec.clone()];
        for stored in airc_trust::load(&self.inner.wire_root).await? {
            peer_specs.push(PeerSpec {
                peer_id: stored.peer_id,
                pubkey: stored.pubkey_bytes()?,
            });
        }
        let self_endpoints = self.route_endpoints()?;
        let endpoints = vec![(self_id, self_endpoints.clone())];
        let mut document = AccountRegistryDocument::from_snapshot(
            &snapshot,
            peer_specs,
            endpoints,
            crate::time::now_ms()?,
        );

        // KEYSTONE (card 0ac…/#35): the publishing node MUST advertise its
        // OWN beacon for same-account cross-machine discovery — that's the
        // entire point of publishing. `from_snapshot` only emits peers in
        // `snapshot.live`, but our own presence is heartbeated for
        // liveness and a registry tick can easily land after its TTL,
        // leaving us in `stale`. The document then went out with peers:0
        // (verified live on bigmama), so a same-account peer reading it
        // discovered nothing — enrol-but-never-route, the keystone's last
        // layer. Stamp a fresh self-beacon (we are definitionally live: we
        // are publishing right now) carrying our dialable endpoints.
        if !document.peers.iter().any(|peer| peer.peer_id() == self_id) {
            // Card cbbcf18d (post-#1146 audit): the stamped self-beacon's
            // channel list comes from `self.subscriptions()` — the LOCAL
            // single source of truth — not from presence-derived
            // `snapshot.live_channels`. In the exact stale-self scenario
            // this branch exists for, `live` is empty, so live_channels
            // is [] even while this scope is subscribed; identity, key,
            // and endpoints already follow the publisher-is-alive
            // doctrine and the channel list must too.
            let subscribed_channels: Vec<ChannelName> = self
                .subscriptions()
                .await?
                .into_iter()
                .map(|subscription| subscription.name)
                .collect();
            for channel in &subscribed_channels {
                if !document.channels.contains(channel) {
                    document.channels.push(channel.clone());
                }
            }
            document.channels.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            let presence = crate::coordinator::beacon_now(
                self_id,
                self.inner.home.clone(),
                subscribed_channels,
                std::process::id(),
                crate::time::now_ms()?,
            );
            document.peers.push(AccountPeerBeacon {
                presence,
                peer_spec: self_spec,
                endpoints: self_endpoints,
            });
            document
                .peers
                .sort_by_key(|peer| peer.peer_id().to_string());
        }
        Ok(document)
    }

    pub async fn publish_account_registry(
        &self,
        store: &dyn AccountRegistryStore,
    ) -> Result<AccountRegistryDocument, AircError> {
        let document = self.account_registry_document().await?;
        store.publish(&document).await?;
        Ok(document)
    }

    pub async fn refresh_account_registry(
        &self,
        store: &dyn AccountRegistryStore,
    ) -> Result<Option<AccountRegistryDocument>, AircError> {
        let identity = self.mesh_identity().await?;
        let Some(document) = store.refresh(&identity).await? else {
            return Ok(None);
        };
        self.import_account_registry_document(document.clone())
            .await?;
        Ok(Some(document))
    }

    /// The peer_ids of LIVE peers in the current fresh account registry
    /// (the merged, stale-pruned document). **READ-ONLY** — unlike
    /// [`Self::refresh_account_registry`], this does NOT enrol/import, so
    /// it is safe to call from a dry run. Returns `None` when no registry
    /// document is available (empty account, gh gate, or no mesh match).
    /// Used by `airc peer prune` as the authoritative "who is alive" set:
    /// an enrolled peer absent from it is a dead-route candidate.
    pub async fn live_registry_peer_ids(
        &self,
        store: &dyn AccountRegistryStore,
    ) -> Result<Option<std::collections::HashSet<airc_core::PeerId>>, AircError> {
        let identity = self.mesh_identity().await?;
        let Some(document) = store.refresh(&identity).await? else {
            return Ok(None);
        };
        Ok(Some(document.peers.iter().map(|p| p.peer_id()).collect()))
    }

    pub async fn import_account_registry_document(
        &self,
        document: AccountRegistryDocument,
    ) -> Result<(), AircError> {
        document.validate().map_err(AircError::AccountRegistry)?;
        for peer in document.peers {
            if peer.peer_id() == self.inner.identity.peer_id {
                continue;
            }
            airc_trust::add(
                &self.inner.wire_root,
                peer.peer_spec.peer_id,
                peer.peer_spec.pubkey,
            )
            .await?;
            // Card 625abe6d slice 1: persist the beacon's endpoints on
            // the trust record so route discovery can dial them after
            // a restart (the in-memory ImportedInviteTable fed below
            // does not survive one). Empty beacons leave the column
            // alone — a registry refresh without endpoints must not
            // wipe endpoints learned elsewhere.
            if !peer.endpoints.is_empty() {
                let endpoints_json = crate::route::endpoints_to_json(&peer.endpoints)
                    .map_err(|error| AircError::Transport(error.to_string()))?;
                airc_trust::set_endpoints_json(
                    &self.inner.wire_root,
                    peer.peer_spec.peer_id,
                    Some(endpoints_json),
                )
                .await?
                // The peer was added to this exact store two lines up;
                // a vanished row here is a structural bug, and
                // endpoints silently not stored is the failure mode
                // this card exists to delete (#1120 sentinel risk note).
                .ok_or_else(|| {
                    AircError::Transport(format!(
                        "peer {} vanished between trust add and endpoint store \
                         during registry import — report as a substrate bug",
                        peer.peer_spec.peer_id
                    ))
                })?;
            }
            self.enrol_volatile_peer(&peer.peer_spec)?;
            crate::coordinator::publish_store(
                self.coordinator_store(),
                &document.mesh_identity,
                &peer.presence,
            )
            .await?;
            self.import_invite_beacon(peer.invite_beacon()).await?;
        }
        self.sync_account_peer_registry().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::PeerId;
    use airc_protocol::PeerKeypair;
    use std::net::SocketAddr;
    use std::path::Path;
    use tempfile::tempdir;

    fn mesh() -> MeshIdentity {
        MeshIdentity::new("joelteply")
    }

    fn channel(name: &str) -> ChannelName {
        ChannelName::new(name).unwrap()
    }

    fn peer_spec(peer_id: PeerId) -> PeerSpec {
        let keypair = PeerKeypair::generate();
        PeerSpec {
            peer_id,
            pubkey: keypair.public_bytes(),
        }
    }

    async fn write_identity(home: &std::path::Path) {
        let store = airc_store::SqliteEventStore::open_path(&home.join("events.sqlite"))
            .await
            .unwrap();
        crate::mesh_identity::resolve_with(
            &store,
            || {
                Some((
                    "joelteply".to_string(),
                    crate::mesh_identity::Source::Operator,
                ))
            },
            4_102_444_800_000,
        )
        .await
        .unwrap();
    }

    #[test]
    fn document_serializes_registry_metadata_not_messages() {
        let peer_id = PeerId::new();
        let presence = crate::coordinator::beacon_now(
            peer_id,
            "/machine/a/.airc".into(),
            vec![channel("general")],
            123,
            1_000,
        );
        let document = AccountRegistryDocument::new(
            mesh(),
            2_000,
            vec![channel("general")],
            vec![AccountPeerBeacon {
                presence,
                peer_spec: peer_spec(peer_id),
                endpoints: vec![RouteEndpoint::LanTcp {
                    addr: SocketAddr::from(([10, 0, 0, 2], 7717)),
                }],
            }],
        );

        let json = serde_json::to_string(&document).unwrap();

        assert!(json.contains("lan_tcp"));
        assert!(!json.contains("message"));
        assert!(!json.contains("transcript"));
        assert!(!json.contains("body"));
    }

    #[tokio::test]
    async fn document_from_snapshot_exports_only_peers_with_specs() {
        let store = airc_store::InMemoryEventStore::new();
        let cfg = crate::coordinator::CoordinatorConfig::default();
        let peer_with_spec = PeerId::new();
        let peer_without_spec = PeerId::new();
        let with_spec = crate::coordinator::beacon_now(
            peer_with_spec,
            "/machine/a/.airc".into(),
            vec![channel("general")],
            123,
            1_000,
        );
        let without_spec = crate::coordinator::beacon_now(
            peer_without_spec,
            "/machine/b/.airc".into(),
            vec![channel("cambriantech")],
            456,
            1_000,
        );
        crate::coordinator::publish_store(&store, &mesh(), &with_spec)
            .await
            .unwrap();
        crate::coordinator::publish_store(&store, &mesh(), &without_spec)
            .await
            .unwrap();
        let snapshot = crate::coordinator::snapshot_store(&store, &mesh(), &cfg, 1_000)
            .await
            .unwrap();

        let document = AccountRegistryDocument::from_snapshot(
            &snapshot,
            vec![peer_spec(peer_with_spec)],
            Vec::<(PeerId, Vec<RouteEndpoint>)>::new(),
            2_000,
        );

        assert_eq!(document.peers.len(), 1);
        assert_eq!(document.peers[0].peer_id(), peer_with_spec);
    }

    #[test]
    fn validation_rejects_peer_spec_mismatch() {
        let presence_peer = PeerId::new();
        let spec_peer = PeerId::new();
        let document = AccountRegistryDocument::new(
            mesh(),
            2_000,
            vec![channel("general")],
            vec![AccountPeerBeacon {
                presence: crate::coordinator::beacon_now(
                    presence_peer,
                    "/machine/a/.airc".into(),
                    vec![channel("general")],
                    123,
                    1_000,
                ),
                peer_spec: peer_spec(spec_peer),
                endpoints: Vec::new(),
            }],
        );

        assert!(matches!(
            document.validate(),
            Err(AccountRegistryError::PeerMismatch { .. })
        ));
    }

    async fn sqlite_registry_store_at(dir: &std::path::Path) -> SqliteAccountRegistryStore {
        let path = dir.join("events.sqlite");
        let event_store = airc_store::SqliteEventStore::open_path(&path)
            .await
            .unwrap();
        SqliteAccountRegistryStore::new(Arc::new(event_store))
    }

    #[tokio::test]
    async fn sqlite_registry_store_publishes_and_refreshes_document() {
        let dir = tempdir().unwrap();
        let store = sqlite_registry_store_at(&dir.path().join("registry")).await;
        let peer_id = PeerId::new();
        let document = AccountRegistryDocument::new(
            mesh(),
            2_000,
            vec![channel("general")],
            vec![AccountPeerBeacon {
                presence: crate::coordinator::beacon_now(
                    peer_id,
                    "/machine/a/.airc".into(),
                    vec![channel("general")],
                    123,
                    1_000,
                ),
                peer_spec: peer_spec(peer_id),
                endpoints: Vec::new(),
            }],
        );

        store.publish(&document).await.unwrap();
        let refreshed = store.refresh(&mesh()).await.unwrap().unwrap();

        assert_eq!(refreshed, document);
    }

    #[tokio::test]
    async fn import_registry_enrols_peer_and_presence() {
        let dir = tempdir().unwrap();
        let machine_a = dir.path().join("machine-a/.airc");
        let machine_b = dir.path().join("machine-b/.airc");
        std::fs::create_dir_all(&machine_a).unwrap();
        std::fs::create_dir_all(&machine_b).unwrap();

        let peer_id = PeerId::new();
        let spec = peer_spec(peer_id);
        let document = AccountRegistryDocument::new(
            mesh(),
            2_000,
            vec![channel("general")],
            vec![AccountPeerBeacon {
                presence: crate::coordinator::beacon_now(
                    peer_id,
                    machine_a.clone(),
                    vec![channel("general")],
                    123,
                    1_000,
                ),
                peer_spec: spec.clone(),
                endpoints: vec![RouteEndpoint::Relay {
                    url: "https://relay.example.test".to_string(),
                }],
            }],
        );

        let airc = Airc::open(&machine_b).await.unwrap();
        airc.import_account_registry_document(document)
            .await
            .unwrap();

        let peers = airc_trust::load(&airc.inner.wire_root).await.unwrap();
        assert!(peers.iter().any(|peer| peer.peer_id == spec.peer_id));
        let snapshot = crate::coordinator::snapshot_store(
            airc.coordinator_store(),
            &mesh(),
            &Default::default(),
            1_000,
        )
        .await
        .unwrap();
        assert!(snapshot
            .live
            .iter()
            .any(|peer| peer.peer_id == spec.peer_id));
        assert_eq!(
            airc.imported_invites().unwrap()[0].endpoints,
            vec![RouteEndpoint::Relay {
                url: "https://relay.example.test".to_string()
            }]
        );
    }

    fn beacon_at(peer_id: PeerId, scope_home: &str, heartbeat_ms: u64) -> AccountPeerBeacon {
        AccountPeerBeacon {
            presence: crate::coordinator::beacon_now(
                peer_id,
                scope_home.into(),
                vec![channel("general")],
                123,
                heartbeat_ms,
            ),
            peer_spec: peer_spec(peer_id),
            endpoints: Vec::new(),
        }
    }

    // Card d793c242: temp-rooted scope homes are the signature of
    // hermetic test daemons — recognized for THIS machine (via
    // std::env::temp_dir) and cross-platform (a beacon's scope_home is
    // minted on another OS). The Windows path below is the literal
    // live-evidence scope_home that polluted the joelteply rendezvous.
    #[test]
    fn temp_scope_home_detection_is_cross_platform() {
        assert!(scope_home_is_temp_rooted(Path::new(
            r"C:\Users\green\AppData\Local\Temp\tmp.YYavgmVUxz\.airc"
        )));
        assert!(scope_home_is_temp_rooted(Path::new("/tmp/airc-test/.airc")));
        assert!(scope_home_is_temp_rooted(Path::new(
            "/private/tmp/scope/.airc"
        )));
        assert!(scope_home_is_temp_rooted(Path::new(
            "/var/folders/ab/c123/T/tmp.xyz/.airc"
        )));
        // This machine's real temp root, however it is spelled.
        let dir = tempdir().unwrap();
        assert!(scope_home_is_temp_rooted(dir.path()));

        // Production shapes must NOT match — the gate may never block
        // a real daemon.
        assert!(!scope_home_is_temp_rooted(Path::new("/Users/joel/.airc")));
        assert!(!scope_home_is_temp_rooted(Path::new(
            "/Users/joel/Development/airc/.airc"
        )));
        assert!(!scope_home_is_temp_rooted(Path::new(
            r"C:\Users\green\.airc"
        )));
        assert!(!scope_home_is_temp_rooted(Path::new("/home/ci/.airc")));
    }

    // Card d793c242 item 3 (reader hygiene): merge drops temp-scoped
    // phantom beacons and COUNTS them. Mutation check: removing the
    // temp filter from merge_registry_documents leaves the phantom in
    // `peers` and zeroes the count — both asserts fail.
    #[test]
    fn merge_ignores_temp_scoped_beacons_and_counts_them() {
        let prod = PeerId::new();
        let phantom_windows = PeerId::new();
        let phantom_unix = PeerId::new();
        let document = AccountRegistryDocument::new(
            mesh(),
            2_000,
            vec![channel("general")],
            vec![
                beacon_at(prod, "/machine/a/.airc", 1_000),
                beacon_at(
                    phantom_windows,
                    r"C:\Users\green\AppData\Local\Temp\tmp.YYavgmVUxz\.airc",
                    1_500,
                ),
                beacon_at(phantom_unix, "/tmp/airc-hermetic/.airc", 1_500),
            ],
        );

        let outcome = merge_registry_documents(vec![document], &mesh());

        assert_eq!(outcome.ignored_temp_beacons, 2);
        let merged = outcome.document.expect("document must merge");
        assert_eq!(merged.peers.len(), 1);
        assert_eq!(merged.peers[0].peer_id(), prod);
    }

    // Card d793c242 item 3: per peer_id the FRESHEST beacon wins
    // across per-machine documents, and peers present in only one
    // document survive the merge (the old pick-newest-document reader
    // dropped them).
    #[test]
    fn merge_prefers_freshest_beacon_per_peer_and_unions_documents() {
        let shared = PeerId::new();
        let only_in_old = PeerId::new();
        let shared_spec = peer_spec(shared);

        let stale = AccountPeerBeacon {
            presence: crate::coordinator::beacon_now(
                shared,
                "/machine/a/.airc".into(),
                vec![channel("general")],
                123,
                1_000,
            ),
            peer_spec: shared_spec.clone(),
            endpoints: vec![RouteEndpoint::Relay {
                url: "https://stale.example.test".to_string(),
            }],
        };
        let fresh = AccountPeerBeacon {
            presence: crate::coordinator::beacon_now(
                shared,
                "/machine/a/.airc".into(),
                vec![channel("general")],
                123,
                5_000,
            ),
            peer_spec: shared_spec,
            endpoints: vec![RouteEndpoint::Relay {
                url: "https://fresh.example.test".to_string(),
            }],
        };
        let old_doc = AccountRegistryDocument::new(
            mesh(),
            2_000,
            vec![channel("general")],
            vec![stale, beacon_at(only_in_old, "/machine/b/.airc", 900)],
        );
        let new_doc = AccountRegistryDocument::new(mesh(), 6_000, vec![], vec![fresh]);

        let outcome = merge_registry_documents(vec![old_doc, new_doc], &mesh());

        let merged = outcome.document.expect("document must merge");
        assert_eq!(outcome.ignored_temp_beacons, 0);
        assert_eq!(merged.generated_at_ms, 6_000);
        assert_eq!(merged.peers.len(), 2, "union across documents");
        let shared_beacon = merged
            .peers
            .iter()
            .find(|peer| peer.peer_id() == shared)
            .expect("shared peer present");
        assert_eq!(
            shared_beacon.endpoints,
            vec![RouteEndpoint::Relay {
                url: "https://fresh.example.test".to_string()
            }],
            "freshest beacon per peer_id must win"
        );
        assert!(merged.peers.iter().any(|p| p.peer_id() == only_in_old));
    }

    // Card 4b6a0ffa / #33 (endpoint-less shadow): when the FRESHEST
    // beacon for a peer carries no endpoints (the manual `registry
    // sync` overwrite shape), the merge keeps the fresh presence but
    // retains the freshest NON-EMPTY endpoint set from an older
    // beacon — a first-contact reader still learns a dialable
    // endpoint. Mutation check: removing the endpoint-retention
    // backfill from merge_registry_documents leaves the merged beacon
    // endpoint-less and the endpoints assert fails.
    #[test]
    fn merge_retains_endpoints_when_fresher_beacon_is_endpointless() {
        let peer = PeerId::new();
        let spec = peer_spec(peer);
        let daemon_beacon = AccountPeerBeacon {
            presence: crate::coordinator::beacon_now(
                peer,
                "/machine/a/.airc".into(),
                vec![channel("general")],
                123,
                1_000,
            ),
            peer_spec: spec.clone(),
            endpoints: vec![RouteEndpoint::LanTcp {
                addr: SocketAddr::from(([10, 0, 0, 2], 7717)),
            }],
        };
        let manual_sync_beacon = AccountPeerBeacon {
            presence: crate::coordinator::beacon_now(
                peer,
                "/machine/a/.airc".into(),
                vec![channel("general")],
                456,
                5_000,
            ),
            peer_spec: spec,
            endpoints: Vec::new(),
        };
        let daemon_doc = AccountRegistryDocument::new(
            mesh(),
            2_000,
            vec![channel("general")],
            vec![daemon_beacon],
        );
        let manual_doc = AccountRegistryDocument::new(
            mesh(),
            6_000,
            vec![channel("general")],
            vec![manual_sync_beacon],
        );

        let outcome = merge_registry_documents(vec![daemon_doc, manual_doc], &mesh());

        let merged = outcome.document.expect("document must merge");
        assert_eq!(merged.peers.len(), 1);
        assert_eq!(
            merged.peers[0].presence.heartbeat_at_ms, 5_000,
            "freshest presence still wins"
        );
        assert_eq!(
            merged.peers[0].endpoints,
            vec![RouteEndpoint::LanTcp {
                addr: SocketAddr::from(([10, 0, 0, 2], 7717)),
            }],
            "endpoint-less fresh beacon must not erase known dialable endpoints"
        );
    }

    #[test]
    fn merge_skips_foreign_mesh_documents() {
        let peer = PeerId::new();
        let foreign = AccountRegistryDocument::new(
            MeshIdentity::new("someone-else"),
            2_000,
            vec![],
            vec![beacon_at(peer, "/machine/a/.airc", 1_000)],
        );
        let outcome = merge_registry_documents(vec![foreign], &mesh());
        assert!(outcome.document.is_none());
    }

    // what this catches: a dead daemon whose freshest surviving gist
    // beacon is days old must be PRUNED from the enrol set, not dialed
    // (the stale-route orphan path the merge alone never closed — it
    // keeps freshest-per-peer but a peer's freshest can still be
    // ancient). The user-flagged "make sure those aren't just old ones".
    // Mutation check: dropping the `is_fresh` filter keeps the stale
    // peer and returns 0 — both asserts fail.
    #[test]
    fn prune_drops_only_peers_staler_than_ttl() {
        let now_ms = 1_000_000;
        let ttl = DEFAULT_PEER_FRESHNESS_TTL_MS; // 600_000
        let fresh = PeerId::new();
        let stale = PeerId::new();
        let future = PeerId::new();
        let mut peers = vec![
            // 100s old — well within the 10min horizon.
            beacon_at(fresh, "/machine/a/.airc", now_ms - 100_000),
            // 900s old — a dead publisher's lingering gist beacon.
            beacon_at(stale, "/machine/b/.airc", now_ms - 900_000),
            // future-dated clock skew — saturating-sub treats as fresh,
            // never pruned (we don't punish a peer for our clock).
            beacon_at(future, "/machine/c/.airc", now_ms + 50_000),
        ];

        let pruned = prune_stale_peers(&mut peers, now_ms, ttl);

        assert_eq!(pruned, 1, "exactly the stale peer is pruned");
        let kept: Vec<_> = peers.iter().map(|p| p.peer_id()).collect();
        assert!(kept.contains(&fresh), "live-but-laggy peer kept");
        assert!(kept.contains(&future), "future-dated peer kept");
        assert!(!kept.contains(&stale), "dead-route peer dropped");
    }

    // Two SEPARATE machine accounts, same gh identity, bridged ONLY
    // through the remote registry. Each machine gets its own EXPLICIT
    // wire root (coordinator store) via `open_with_wire_root_for_test`,
    // so `machine_account_home`/`HOME` never collapses them onto one
    // store — no process-global env mutation (which would race the
    // parallel test runner), and identical behavior on Unix and Windows.
    #[tokio::test]
    async fn sqlite_registry_bridges_two_isolated_machine_homes() {
        let dir = tempdir().unwrap();
        let machine_a = dir.path().join("machine-a/.airc");
        let machine_b = dir.path().join("machine-b/.airc");
        let wire_a = dir.path().join("wire-a");
        let wire_b = dir.path().join("wire-b");
        // Seed each machine's coordinator (wire-root) store with the
        // shared gh identity so mesh resolution is deterministic.
        write_identity(&wire_a).await;
        write_identity(&wire_b).await;
        let store = sqlite_registry_store_at(&dir.path().join("remote-registry")).await;

        // Machine A publishes its presence/registry to the remote store.
        let airc_a = Airc::open_with_wire_root_for_test(&machine_a, &wire_a)
            .await
            .unwrap();
        airc_a.join("general").await.unwrap();
        airc_a.publish_account_registry(&store).await.unwrap();

        // Machine B refreshes from the remote store — airc_a's beacon
        // reaches B's coordinator ONLY through this bridge (separate
        // wire roots, so it cannot leak via a shared store).
        let airc_b = Airc::open_with_wire_root_for_test(&machine_b, &wire_b)
            .await
            .unwrap();
        let refreshed = airc_b.refresh_account_registry(&store).await.unwrap();
        assert!(refreshed.is_some());

        let peers = airc_trust::load(&airc_b.inner.wire_root).await.unwrap();
        assert!(peers.iter().any(|peer| peer.peer_id == airc_a.peer_id()));
        let snapshot = crate::coordinator::snapshot_store(
            airc_b.coordinator_store(),
            &mesh(),
            &Default::default(),
            u64::MAX,
        )
        .await
        .unwrap();
        assert!(snapshot
            .stale
            .iter()
            .any(|peer| peer.peer_id == airc_a.peer_id()));
    }

    // THE KEYSTONE PIN (card cbbcf18d, post-#1146 audit — the mutation
    // that SURVIVED). The #1146 fix stamps a fresh self-beacon into the
    // published document when this peer's own presence heartbeat has
    // outlived the coordinator TTL (`snapshot.stale`, NOT `live`).
    // Every pre-existing publish-path test joined a channel immediately
    // before publishing, keeping self coordinator-live and bypassing
    // the fix's branch entirely — so removing the branch left all
    // tests green. This test reproduces the bug's ACTUAL trigger:
    // self's beacon is past-TTL at document time.
    //
    // Mutation checks (both verified):
    //   1. bypass the self-insertion branch in
    //      `account_registry_document` (`if false && …`) → the
    //      exactly-one-self-beacon assert fails;
    //   2. revert the stamped beacon's channels to presence-derived
    //      `snapshot.live_channels` → the subscribed_channels assert
    //      fails (live is empty here, subscriptions are not).
    #[tokio::test]
    async fn stale_self_publish_stamps_one_self_beacon_with_endpoints_and_channels() {
        let dir = tempdir().unwrap();
        let machine = dir.path().join("machine/.airc");
        let wire = dir.path().join("wire");
        write_identity(&wire).await;
        let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
            .await
            .unwrap();

        // Subscribe (local SoT: subscriptions exist) and give self a
        // dialable endpoint, as the daemon's registry glue does.
        airc.join("general").await.unwrap();
        airc.upsert_route_endpoint(RouteEndpoint::Relay {
            url: "https://self.example.test".to_string(),
        })
        .unwrap();

        // Overwrite self's presence with a PAST-TTL heartbeat — the
        // registry tick landing after the heartbeat TTL, the exact
        // trigger verified live on bigmama. heartbeat_at_ms=1_000 is
        // ~56 years past any real `now_ms`, far beyond the 60s TTL.
        let stale_self = crate::coordinator::beacon_now(
            airc.peer_id(),
            machine.clone(),
            vec![channel("general")],
            std::process::id(),
            1_000,
        );
        crate::coordinator::publish_store(airc.coordinator_store(), &mesh(), &stale_self)
            .await
            .unwrap();

        // Sanity: self must be STALE, not live — otherwise this test
        // degenerates into the bypassing shape the audit flagged.
        let snapshot = crate::coordinator::snapshot_store(
            airc.coordinator_store(),
            &mesh(),
            &crate::coordinator::CoordinatorConfig::default(),
            crate::time::now_ms().unwrap(),
        )
        .await
        .unwrap();
        assert!(
            snapshot.live.iter().all(|p| p.peer_id != airc.peer_id()),
            "precondition: self must NOT be coordinator-live"
        );
        assert!(
            snapshot.stale.iter().any(|p| p.peer_id == airc.peer_id()),
            "precondition: self must be in snapshot.stale"
        );

        let document = airc.account_registry_document().await.unwrap();

        let self_beacons: Vec<_> = document
            .peers
            .iter()
            .filter(|peer| peer.peer_id() == airc.peer_id())
            .collect();
        assert_eq!(
            self_beacons.len(),
            1,
            "stale-self publish must stamp exactly one self-beacon"
        );
        let self_beacon = self_beacons[0];
        let expected_endpoints = airc.route_endpoints().unwrap();
        assert!(
            !expected_endpoints.is_empty(),
            "precondition: this handle has endpoints to advertise"
        );
        assert_eq!(
            self_beacon.endpoints, expected_endpoints,
            "self-beacon must carry route_endpoints()"
        );
        // Card cbbcf18d item 2: channels from the LOCAL SoT
        // (self.subscriptions()), not presence-derived live_channels
        // (empty in this exact scenario).
        assert_eq!(
            self_beacon.presence.subscribed_channels,
            vec![channel("general")],
            "stamped self-beacon channels must come from self.subscriptions()"
        );
        assert!(
            document.channels.contains(&channel("general")),
            "document channels must include the local subscriptions"
        );
    }
}
