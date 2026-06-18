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

/// Card 625abe6d slice 1 — encode endpoints for the opaque
/// `peer_trust.endpoints_json` column. The store crate sits below
/// this one in the dependency graph and must not know the variants,
/// so the typed boundary lives here with the enum.
pub fn endpoints_to_json(endpoints: &[RouteEndpoint]) -> Result<String, serde_json::Error> {
    serde_json::to_string(endpoints)
}

/// Inverse of [`endpoints_to_json`]. A decode failure is surfaced,
/// never swallowed — a peer record carrying endpoint JSON this binary
/// can't read is version skew the operator must see, not an empty
/// route list.
pub fn endpoints_from_json(json: &str) -> Result<Vec<RouteEndpoint>, serde_json::Error> {
    serde_json::from_str(json)
}

impl RouteEndpoint {
    /// Parse the operator-facing endpoint syntax used by the dev verb
    /// `airc peer add --endpoint`:
    ///
    /// - `lan-tcp:HOST:PORT`
    /// - `tailscale-tcp:HOST:PORT`
    /// - `udp:HOST:PORT`
    /// - `relay:URL`
    ///
    /// Errors name the supported forms — this string arrives from a
    /// human (or an agent quoting a human), so the failure message is
    /// the documentation.
    pub fn parse_cli(input: &str) -> Result<Self, String> {
        let (kind, rest) = input.split_once(':').ok_or_else(|| {
            format!(
                "endpoint {input:?} has no kind prefix; expected \
                 lan-tcp:HOST:PORT, tailscale-tcp:HOST:PORT, udp:HOST:PORT, or relay:URL"
            )
        })?;
        match kind {
            "lan-tcp" | "tailscale-tcp" | "udp" => {
                let addr: SocketAddr = rest.parse().map_err(|error| {
                    format!("endpoint {input:?}: {rest:?} is not a valid HOST:PORT: {error}")
                })?;
                Ok(match kind {
                    "lan-tcp" => RouteEndpoint::LanTcp { addr },
                    "tailscale-tcp" => RouteEndpoint::TailscaleTcp { addr },
                    _ => RouteEndpoint::Udp { addr },
                })
            }
            "relay" => Ok(RouteEndpoint::Relay {
                url: rest.to_string(),
            }),
            other => Err(format!(
                "endpoint kind {other:?} not supported by --endpoint; expected \
                 lan-tcp, tailscale-tcp, udp, or relay"
            )),
        }
    }

    /// Build a relay endpoint whose URL carries BOTH the relay's dialable
    /// address AND its pinned peer id — `airc-relay://<peer>@<addr>`. A
    /// peer importing this endpoint can then dial AND authenticate the
    /// relay (the mTLS pin in `connect_relay`) with no separate
    /// out-of-band credential exchange — closing the gap that left
    /// `Relay { url }` un-connectable from discovery (#1247 slice 1).
    pub fn relay(relay_peer: PeerId, relay_addr: SocketAddr) -> Self {
        RouteEndpoint::Relay {
            url: format!("airc-relay://{relay_peer}@{relay_addr}"),
        }
    }

    /// If this is a relay endpoint whose URL carries a pinned peer id and
    /// a dialable address, return the `(peer, addr)` pair
    /// [`Airc::connect_relay`](crate::Airc::connect_relay) needs.
    ///
    /// `None` for a non-relay endpoint OR a relay URL missing either half
    /// (e.g. a legacy `airc-relay://<addr>` with no peer id) — discovery
    /// surfaces that as "relay advertised but not connectable" rather
    /// than dialing an unauthenticated relay.
    pub fn connectable_relay(&self) -> Option<(PeerId, SocketAddr)> {
        let RouteEndpoint::Relay { url } = self else {
            return None;
        };
        let rest = url.strip_prefix("airc-relay://")?;
        let (peer, addr) = rest.rsplit_once('@')?;
        let peer = PeerId::from_uuid(uuid::Uuid::parse_str(peer).ok()?);
        let addr = addr.parse::<SocketAddr>().ok()?;
        Some((peer, addr))
    }
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

    /// Record an endpoint this handle advertises (one slot per
    /// endpoint kind, latest wins). Public since card 4b6a0ffa (#33):
    /// the manual `registry sync` CLI seeds its short-lived handle
    /// with the DAEMON's read-back endpoints before publishing, so
    /// the account beacon carries a dialable address instead of an
    /// endpoint-less overwrite.
    pub fn upsert_route_endpoint(&self, endpoint: RouteEndpoint) -> Result<(), AircError> {
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

    /// what this catches (#1247 slice 1): a relay endpoint built with a
    /// pinned peer id round-trips back to the exact `(peer, addr)` pair
    /// `connect_relay` needs — so a relay advertised through the gist is
    /// dialable AND pinnable with no out-of-band credential exchange.
    #[test]
    fn relay_endpoint_carries_peer_id_and_round_trips() {
        let relay_peer = PeerId::new();
        let relay_addr = SocketAddr::from(([10, 0, 1, 16], 65458));
        let endpoint = RouteEndpoint::relay(relay_peer, relay_addr);

        let (peer, addr) = endpoint
            .connectable_relay()
            .expect("relay endpoint with a peer id must be connectable");
        assert_eq!(peer, relay_peer);
        assert_eq!(addr, relay_addr);

        // Survives JSON (the gist/trust-store boundary).
        let json = endpoints_to_json(std::slice::from_ref(&endpoint)).expect("json");
        let back = endpoints_from_json(&json).expect("decode");
        assert_eq!(back[0].connectable_relay(), Some((relay_peer, relay_addr)));
    }

    /// what this catches: a legacy `airc-relay://<addr>` URL (no peer id)
    /// is NOT connectable — discovery must surface "advertised but not
    /// pinnable" rather than dial an unauthenticated relay. Likewise a
    /// non-relay endpoint yields `None`.
    #[test]
    fn relay_without_peer_id_or_non_relay_is_not_connectable() {
        let legacy = RouteEndpoint::Relay {
            url: "airc-relay://10.0.1.16:65458".to_string(),
        };
        assert_eq!(legacy.connectable_relay(), None);

        let lan = RouteEndpoint::LanTcp {
            addr: SocketAddr::from(([127, 0, 0, 1], 7474)),
        };
        assert_eq!(lan.connectable_relay(), None);
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
