//! Route discovery and health ingestion.
//!
//! Discovery reads substrate-owned state and turns it into route
//! health/endpoints. Consumers should not hand-edit route health just
//! to make normal local/LAN operation work.

use airc_core::PeerId;

use crate::error::AircError;
use crate::route::health::TransportHealthSample;
use crate::route::invite::RouteEndpoint;
use crate::route::policy::TransportKind;
use crate::Airc;

/// Card 625abe6d slice 1 — a stored peer endpoint that could not be
/// dialed during discovery. Surfaced on the snapshot (and printed by
/// `airc transport health`) instead of being swallowed: an offline
/// peer is normal, but the operator must be able to SEE that a dial
/// was attempted and why it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDialFailure {
    pub peer_id: PeerId,
    pub endpoint: RouteEndpoint,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDiscoverySnapshot {
    pub health: Vec<TransportHealthSample>,
    pub endpoints: Vec<RouteEndpoint>,
    pub connected_lan_peers: Vec<PeerId>,
    /// Card 625abe6d slice 1: outbound dial attempts to stored peer
    /// endpoints that failed this refresh. Empty when every stored
    /// endpoint either connected or was already connected.
    pub peer_dial_failures: Vec<PeerDialFailure>,
}

impl Airc {
    /// Refresh route health from substrate-owned discovery state.
    ///
    /// This does not invent Tailscale, relay, UDP, or WebRTC routes.
    /// Those adapters feed this table when their discovery/probes
    /// exist. Today it ingests:
    ///
    /// - local-fs: always healthy for the current local store/wire path
    /// - LAN-TCP: healthy when there is a bound LAN endpoint or an
    ///   active LAN peer connection
    ///
    /// Card 625abe6d slice 1: discovery also DIALS — every enrolled
    /// peer whose trust record carries endpoints gets an outbound
    /// connect attempt (cost order: LAN first, then tailscale) unless
    /// already connected. Outbound-only doctrine: this is the path by
    /// which a firewalled node reaches the mesh without ever opening
    /// an inbound port. Dial failures are recorded on the snapshot,
    /// never swallowed.
    pub async fn refresh_route_discovery(&self) -> Result<RouteDiscoverySnapshot, AircError> {
        let peer_dial_failures = self.dial_stored_peer_endpoints().await?;

        let endpoints = self.route_endpoints()?;
        let lan_has_endpoint = endpoints
            .iter()
            .any(|endpoint| matches!(endpoint, RouteEndpoint::LanTcp { .. }));
        let connected_lan_peers = self.connected_lan_peers().await;
        if lan_has_endpoint || !connected_lan_peers.is_empty() {
            self.upsert_transport_health(TransportHealthSample::healthy_direct(
                TransportKind::LanTcp,
            ))?;
        }

        Ok(RouteDiscoverySnapshot {
            health: self.transport_health()?,
            endpoints,
            connected_lan_peers,
            peer_dial_failures,
        })
    }

    /// Card 625abe6d slice 1 — outbound-dial every enrolled peer with
    /// stored endpoints. One successful connect per peer is enough
    /// (first endpoint to answer wins, in stored = cost order); a peer
    /// already connected over LAN is skipped entirely.
    ///
    /// Endpoint JSON that fails to decode is a hard error (version
    /// skew the operator must see), per the no-silent-fallback rule.
    /// CONNECTION failures are not errors — offline peers are a normal
    /// mesh state — but every failed attempt is returned for display.
    async fn dial_stored_peer_endpoints(&self) -> Result<Vec<PeerDialFailure>, AircError> {
        // Both registries: the scope's own store (where the CLI's
        // `peer add --endpoint` writes) and the machine-account wire
        // root (where account-registry import writes). Mirrors
        // load_peer_registries' two-store union in airc.rs.
        let mut stored = airc_trust::load(&self.inner.home)
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        if self.inner.wire_root != self.inner.home {
            stored.extend(
                airc_trust::load(&self.inner.wire_root)
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))?,
            );
        }
        let connected: std::collections::HashSet<PeerId> =
            self.connected_lan_peers().await.into_iter().collect();
        let mut failures = Vec::new();
        // A peer enrolled in both stores appears twice in the union;
        // dial each peer at most once per refresh.
        let mut seen = std::collections::HashSet::new();
        for peer in stored {
            if peer.peer_id == self.inner.identity.peer_id {
                continue;
            }
            if connected.contains(&peer.peer_id) || !seen.insert(peer.peer_id) {
                continue;
            }
            let Some(json) = peer.endpoints_json.as_deref() else {
                continue;
            };
            let endpoints = crate::route::endpoints_from_json(json).map_err(|error| {
                AircError::Transport(format!(
                    "peer {} has endpoint JSON this binary cannot decode \
                     (version skew?): {error}",
                    peer.peer_id
                ))
            })?;
            for endpoint in endpoints {
                let addr = match endpoint {
                    RouteEndpoint::LanTcp { addr } | RouteEndpoint::TailscaleTcp { addr } => addr,
                    // Relay/UDP/WebRTC/Reticulum dialing lands in later
                    // slices; recording them as failures here would be
                    // noise about unimplemented transports, not signal
                    // about unreachable peers.
                    _ => continue,
                };
                match self.connect_lan(addr, peer.peer_id).await {
                    Ok(()) => break,
                    Err(error) => failures.push(PeerDialFailure {
                        peer_id: peer.peer_id,
                        endpoint,
                        error: error.to_string(),
                    }),
                }
            }
        }
        Ok(failures)
    }

    /// Snapshot the last known discovery state without mutating
    /// route health.
    pub async fn route_discovery_snapshot(&self) -> Result<RouteDiscoverySnapshot, AircError> {
        Ok(RouteDiscoverySnapshot {
            health: self.transport_health()?,
            endpoints: self.route_endpoints()?,
            connected_lan_peers: self.connected_lan_peers().await,
            peer_dial_failures: Vec::new(),
        })
    }

    async fn connected_lan_peers(&self) -> Vec<PeerId> {
        let adapter = self.inner.lan_tcp.lock().await.clone();
        match adapter {
            Some(adapter) => adapter.connected_peers().await,
            None => Vec::new(),
        }
    }
}
