//! Invite/rendezvous payloads.
//!
//! Gists and other public rendezvous mechanisms should publish this
//! kind of connection metadata, not runtime messages. Runtime traffic
//! moves over admitted live transports after a peer consumes the
//! invite.

use std::net::SocketAddr;

use airc_core::PeerId;
use airc_transport::GhGistInviteStore;
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::registry::PeerSpec;
use crate::Airc;

pub const INVITE_BEACON_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteEndpoint {
    LanTcp { addr: SocketAddr },
    TailscaleTcp { addr: SocketAddr },
    Udp { addr: SocketAddr },
    Relay { url: String },
    Reticulum { destination: String },
    WebRtcSignaling { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteBeacon {
    pub schema_version: u16,
    pub peer_id: PeerId,
    pub peer_spec: PeerSpec,
    pub endpoints: Vec<RouteEndpoint>,
}

impl InviteBeacon {
    pub fn new(peer_id: PeerId, peer_spec: PeerSpec, endpoints: Vec<RouteEndpoint>) -> Self {
        Self {
            schema_version: INVITE_BEACON_SCHEMA_VERSION,
            peer_id,
            peer_spec,
            endpoints,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RouteEndpointTable {
    endpoints: dashmap::DashMap<RouteEndpointKind, RouteEndpoint>,
}

impl RouteEndpointTable {
    pub fn upsert(&self, endpoint: RouteEndpoint) {
        self.endpoints
            .insert(RouteEndpointKind::from(&endpoint), endpoint);
    }

    pub fn endpoints(&self) -> Vec<RouteEndpoint> {
        let mut endpoints = self
            .endpoints
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        endpoints.sort_by_key(|endpoint| RouteEndpointKind::from(endpoint));
        endpoints
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedInvite {
    pub peer_id: PeerId,
    pub endpoints: Vec<RouteEndpoint>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportedInviteTable {
    invites: dashmap::DashMap<PeerId, ImportedInvite>,
}

impl ImportedInviteTable {
    pub fn import(&self, beacon: InviteBeacon) {
        let imported = ImportedInvite {
            peer_id: beacon.peer_id,
            endpoints: beacon.endpoints,
        };
        self.invites.insert(imported.peer_id, imported);
    }

    pub fn invites(&self) -> Vec<ImportedInvite> {
        let mut invites = self
            .invites
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<_>>();
        invites.sort_by_key(|invite| invite.peer_id.to_string());
        invites
    }
}

impl Airc {
    pub fn route_endpoints(&self) -> Result<Vec<RouteEndpoint>, AircError> {
        Ok(self.inner.route_endpoints.endpoints())
    }

    pub fn invite_beacon(&self) -> Result<InviteBeacon, AircError> {
        Ok(InviteBeacon::new(
            self.inner.identity.peer_id,
            PeerSpec {
                peer_id: self.inner.identity.peer_id,
                pubkey: self.inner.identity.keypair.public_bytes(),
            },
            self.route_endpoints()?,
        ))
    }

    pub async fn import_invite_beacon(&self, beacon: InviteBeacon) -> Result<(), AircError> {
        let peer_spec = beacon.peer_spec.clone();
        self.add_peer_via(peer_spec, "invite").await?;
        self.inner.imported_invites.import(beacon);
        Ok(())
    }

    pub fn imported_invites(&self) -> Result<Vec<ImportedInvite>, AircError> {
        Ok(self.inner.imported_invites.invites())
    }

    pub async fn publish_gist_invite(&self, gist_id: &str) -> Result<InviteBeacon, AircError> {
        let beacon = self.invite_beacon()?;
        GhGistInviteStore::new(gist_id)
            .publish(&beacon)
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        Ok(beacon)
    }

    pub async fn import_gist_invite(
        &self,
        gist_id: &str,
    ) -> Result<Option<InviteBeacon>, AircError> {
        let Some(beacon) = GhGistInviteStore::new(gist_id)
            .read::<InviteBeacon>()
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?
        else {
            return Ok(None);
        };
        self.import_invite_beacon(beacon.clone()).await?;
        Ok(Some(beacon))
    }

    pub(crate) fn upsert_route_endpoint(&self, endpoint: RouteEndpoint) -> Result<(), AircError> {
        self.inner.route_endpoints.upsert(endpoint);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum RouteEndpointKind {
    LanTcp,
    TailscaleTcp,
    Udp,
    Relay,
    Reticulum,
    WebRtcSignaling,
}

impl From<&RouteEndpoint> for RouteEndpointKind {
    fn from(endpoint: &RouteEndpoint) -> Self {
        match endpoint {
            RouteEndpoint::LanTcp { .. } => Self::LanTcp,
            RouteEndpoint::TailscaleTcp { .. } => Self::TailscaleTcp,
            RouteEndpoint::Udp { .. } => Self::Udp,
            RouteEndpoint::Relay { .. } => Self::Relay,
            RouteEndpoint::Reticulum { .. } => Self::Reticulum,
            RouteEndpoint::WebRtcSignaling { .. } => Self::WebRtcSignaling,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_protocol::PeerKeypair;

    #[test]
    fn invite_beacon_serializes_connection_metadata_without_messages() {
        let keypair = PeerKeypair::generate();
        let peer_id = PeerId::new();
        let beacon = InviteBeacon::new(
            peer_id,
            PeerSpec {
                peer_id,
                pubkey: keypair.public_bytes(),
            },
            vec![RouteEndpoint::LanTcp {
                addr: SocketAddr::from(([127, 0, 0, 1], 7474)),
            }],
        );

        let json = serde_json::to_string(&beacon).expect("beacon json");

        assert!(json.contains("lan_tcp"));
        assert!(json.contains("127.0.0.1:7474"));
        assert!(!json.contains("message"));
    }

    #[test]
    fn endpoint_table_replaces_same_transport_kind() {
        let table = RouteEndpointTable::default();
        table.upsert(RouteEndpoint::LanTcp {
            addr: SocketAddr::from(([127, 0, 0, 1], 1000)),
        });
        table.upsert(RouteEndpoint::LanTcp {
            addr: SocketAddr::from(([127, 0, 0, 1], 2000)),
        });

        assert_eq!(
            table.endpoints(),
            vec![RouteEndpoint::LanTcp {
                addr: SocketAddr::from(([127, 0, 0, 1], 2000))
            }]
        );
    }

    #[test]
    fn endpoint_table_tracks_udp_separately_from_lan_tcp() {
        let table = RouteEndpointTable::default();
        table.upsert(RouteEndpoint::LanTcp {
            addr: SocketAddr::from(([127, 0, 0, 1], 1000)),
        });
        table.upsert(RouteEndpoint::Udp {
            addr: SocketAddr::from(([127, 0, 0, 1], 1000)),
        });

        assert_eq!(
            table.endpoints(),
            vec![
                RouteEndpoint::LanTcp {
                    addr: SocketAddr::from(([127, 0, 0, 1], 1000))
                },
                RouteEndpoint::Udp {
                    addr: SocketAddr::from(([127, 0, 0, 1], 1000))
                }
            ]
        );
    }

    #[test]
    fn imported_invites_are_remote_not_local_advertised_endpoints() {
        let keypair = PeerKeypair::generate();
        let peer_id = PeerId::new();
        let table = ImportedInviteTable::default();
        table.import(InviteBeacon::new(
            peer_id,
            PeerSpec {
                peer_id,
                pubkey: keypair.public_bytes(),
            },
            vec![RouteEndpoint::Relay {
                url: "https://relay.example".to_string(),
            }],
        ));

        assert_eq!(
            table.invites(),
            vec![ImportedInvite {
                peer_id,
                endpoints: vec![RouteEndpoint::Relay {
                    url: "https://relay.example".to_string()
                }]
            }]
        );
    }
}
