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
        let mut peer_specs = vec![PeerSpec {
            peer_id: self.inner.identity.peer_id,
            pubkey: self.inner.identity.keypair.public_bytes(),
        }];
        for stored in airc_trust::load(&self.inner.wire_root).await? {
            peer_specs.push(PeerSpec {
                peer_id: stored.peer_id,
                pubkey: stored.pubkey_bytes()?,
            });
        }
        let endpoints = vec![(self.inner.identity.peer_id, self.route_endpoints()?)];
        Ok(AccountRegistryDocument::from_snapshot(
            &snapshot,
            peer_specs,
            endpoints,
            crate::time::now_ms()?,
        ))
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
}
