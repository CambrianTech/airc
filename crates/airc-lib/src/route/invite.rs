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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteEndpointTable {
    endpoints: Vec<RouteEndpoint>,
}

impl RouteEndpointTable {
    pub fn upsert(&mut self, endpoint: RouteEndpoint) {
        match self
            .endpoints
            .iter_mut()
            .find(|existing| same_endpoint_kind(existing, &endpoint))
        {
            Some(existing) => *existing = endpoint,
            None => self.endpoints.push(endpoint),
        }
    }

    pub fn endpoints(&self) -> Vec<RouteEndpoint> {
        self.endpoints.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedInvite {
    pub peer_id: PeerId,
    pub endpoints: Vec<RouteEndpoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedInviteTable {
    invites: Vec<ImportedInvite>,
}

impl ImportedInviteTable {
    pub fn import(&mut self, beacon: InviteBeacon) {
        let imported = ImportedInvite {
            peer_id: beacon.peer_id,
            endpoints: beacon.endpoints,
        };
        match self
            .invites
            .iter_mut()
            .find(|existing| existing.peer_id == imported.peer_id)
        {
            Some(existing) => *existing = imported,
            None => self.invites.push(imported),
        }
    }

    pub fn invites(&self) -> Vec<ImportedInvite> {
        self.invites.clone()
    }
}

impl Airc {
    pub fn route_endpoints(&self) -> Result<Vec<RouteEndpoint>, AircError> {
        self.inner
            .route_endpoints
            .read()
            .map_err(|_| AircError::Route("route endpoints lock poisoned".to_string()))
            .map(|table| table.endpoints())
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
        let mut invites = self
            .inner
            .imported_invites
            .write()
            .map_err(|_| AircError::Route("imported invites lock poisoned".to_string()))?;
        invites.import(beacon);
        Ok(())
    }

    pub fn imported_invites(&self) -> Result<Vec<ImportedInvite>, AircError> {
        self.inner
            .imported_invites
            .read()
            .map_err(|_| AircError::Route("imported invites lock poisoned".to_string()))
            .map(|table| table.invites())
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
        let mut endpoints = self
            .inner
            .route_endpoints
            .write()
            .map_err(|_| AircError::Route("route endpoints lock poisoned".to_string()))?;
        endpoints.upsert(endpoint);
        Ok(())
    }
}

fn same_endpoint_kind(left: &RouteEndpoint, right: &RouteEndpoint) -> bool {
    matches!(
        (left, right),
        (RouteEndpoint::LanTcp { .. }, RouteEndpoint::LanTcp { .. })
            | (
                RouteEndpoint::TailscaleTcp { .. },
                RouteEndpoint::TailscaleTcp { .. }
            )
            | (RouteEndpoint::Udp { .. }, RouteEndpoint::Udp { .. })
            | (RouteEndpoint::Relay { .. }, RouteEndpoint::Relay { .. })
            | (
                RouteEndpoint::Reticulum { .. },
                RouteEndpoint::Reticulum { .. }
            )
            | (
                RouteEndpoint::WebRtcSignaling { .. },
                RouteEndpoint::WebRtcSignaling { .. }
            )
    )
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
        let mut table = RouteEndpointTable::default();
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
        let mut table = RouteEndpointTable::default();
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
        let mut table = ImportedInviteTable::default();
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
