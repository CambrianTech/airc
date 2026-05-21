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
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::coordinator::{CoordinatorSnapshot, PresenceBeacon};
use crate::error::AircError;
use crate::registry::PeerSpec;
use crate::route::{InviteBeacon, RouteEndpoint};
use crate::subscriptions::{ChannelName, MeshIdentity};
use crate::Airc;

pub const ACCOUNT_REGISTRY_SCHEMA_VERSION: u16 = 1;
const REGISTRY_FILENAME: &str = "registry.json";

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

#[derive(Debug, Clone)]
pub struct FileAccountRegistryStore {
    root: PathBuf,
}

impl FileAccountRegistryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, mesh_identity: &MeshIdentity) -> PathBuf {
        self.root
            .join(safe_component(mesh_identity.as_str()))
            .join(REGISTRY_FILENAME)
    }
}

#[async_trait]
impl AccountRegistryStore for FileAccountRegistryStore {
    async fn publish(
        &self,
        document: &AccountRegistryDocument,
    ) -> Result<(), AccountRegistryError> {
        document.validate()?;
        let path = self.path_for(&document.mesh_identity);
        let parent = path
            .parent()
            .ok_or_else(|| AccountRegistryError::Adapter("registry path has no parent".into()))?;
        fs::create_dir_all(parent).map_err(|error| {
            AccountRegistryError::Adapter(format!(
                "create registry dir {}: {error}",
                parent.display()
            ))
        })?;
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(document).map_err(|error| {
            AccountRegistryError::Adapter(format!("serialize registry document: {error}"))
        })?;
        fs::write(&tmp, text).map_err(|error| {
            AccountRegistryError::Adapter(format!("write registry tmp {}: {error}", tmp.display()))
        })?;
        fs::rename(&tmp, &path).map_err(|error| {
            AccountRegistryError::Adapter(format!(
                "publish registry {} -> {}: {error}",
                tmp.display(),
                path.display()
            ))
        })?;
        Ok(())
    }

    async fn refresh(
        &self,
        mesh_identity: &MeshIdentity,
    ) -> Result<Option<AccountRegistryDocument>, AccountRegistryError> {
        let path = self.path_for(mesh_identity);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AccountRegistryError::Adapter(format!(
                    "read registry {}: {error}",
                    path.display()
                )));
            }
        };
        let document: AccountRegistryDocument = serde_json::from_str(&text).map_err(|error| {
            AccountRegistryError::Adapter(format!("parse registry {}: {error}", path.display()))
        })?;
        document.validate()?;
        Ok(Some(document))
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

impl Airc {
    pub fn account_registry_document(&self) -> Result<AccountRegistryDocument, AircError> {
        let identity = self.mesh_identity()?;
        let snapshot = crate::coordinator::snapshot(
            &self.inner.wire_root,
            &identity,
            &crate::coordinator::CoordinatorConfig::default(),
            crate::time::now_ms()?,
        )?;
        let mut peer_specs = vec![PeerSpec {
            peer_id: self.inner.identity.peer_id,
            pubkey: self.inner.identity.keypair.public_bytes(),
        }];
        for stored in airc_daemon::peers_store::load(&self.inner.wire_root)? {
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
        let document = self.account_registry_document()?;
        store.publish(&document).await?;
        Ok(document)
    }

    pub async fn refresh_account_registry(
        &self,
        store: &dyn AccountRegistryStore,
    ) -> Result<Option<AccountRegistryDocument>, AircError> {
        let identity = self.mesh_identity()?;
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
            airc_daemon::peers_store::add(
                &self.inner.wire_root,
                peer.peer_spec.peer_id,
                peer.peer_spec.pubkey,
            )?;
            self.enrol_volatile_peer(&peer.peer_spec)?;
            crate::coordinator::publish(
                &self.inner.wire_root,
                &document.mesh_identity,
                &peer.presence,
            )?;
            self.import_invite_beacon(peer.invite_beacon()).await?;
        }
        self.sync_account_peer_registry()?;
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

    fn write_identity(home: &std::path::Path) {
        std::fs::create_dir_all(home).unwrap();
        std::fs::write(
            home.join("mesh_identity.json"),
            r#"{
  "version": 1,
  "identity": "joelteply",
  "source": "operator",
  "resolved_at_ms": 4102444800000,
  "ttl_ms": 86400000
}
"#,
        )
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

    #[test]
    fn document_from_snapshot_exports_only_peers_with_specs() {
        let dir = tempdir().unwrap();
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
        crate::coordinator::publish(dir.path(), &mesh(), &with_spec).unwrap();
        crate::coordinator::publish(dir.path(), &mesh(), &without_spec).unwrap();
        let snapshot = crate::coordinator::snapshot(dir.path(), &mesh(), &cfg, 1_000).unwrap();

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

    #[tokio::test]
    async fn file_registry_store_publishes_and_refreshes_document() {
        let dir = tempdir().unwrap();
        let store = FileAccountRegistryStore::new(dir.path().join("registry"));
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
        assert!(store.root().join("joelteply/registry.json").is_file());
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

        let peers = airc_daemon::peers_store::load(&airc.inner.wire_root).unwrap();
        assert!(peers.iter().any(|peer| peer.peer_id == spec.peer_id));
        let snapshot = crate::coordinator::snapshot(
            &airc.inner.wire_root,
            &mesh(),
            &Default::default(),
            1_000,
        )
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

    #[tokio::test]
    async fn file_registry_bridges_two_isolated_machine_homes() {
        let dir = tempdir().unwrap();
        let machine_a = dir.path().join("machine-a/.airc");
        let machine_b = dir.path().join("machine-b/.airc");
        write_identity(&machine_a);
        write_identity(&machine_b);
        let store = FileAccountRegistryStore::new(dir.path().join("remote-registry"));

        let airc_a = Airc::open(&machine_a).await.unwrap();
        airc_a.join("general").await.unwrap();
        airc_a.publish_account_registry(&store).await.unwrap();

        let airc_b = Airc::open(&machine_b).await.unwrap();
        let refreshed = airc_b.refresh_account_registry(&store).await.unwrap();

        assert!(refreshed.is_some());
        let peers = airc_daemon::peers_store::load(&airc_b.inner.wire_root).unwrap();
        assert!(peers.iter().any(|peer| peer.peer_id == airc_a.peer_id()));
        let snapshot = crate::coordinator::snapshot(
            &airc_b.inner.wire_root,
            &mesh(),
            &Default::default(),
            u64::MAX,
        )
        .unwrap();
        assert!(snapshot
            .stale
            .iter()
            .any(|peer| peer.peer_id == airc_a.peer_id()));
    }
}
