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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDiscoverySnapshot {
    pub health: Vec<TransportHealthSample>,
    pub endpoints: Vec<RouteEndpoint>,
    pub connected_lan_peers: Vec<PeerId>,
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
    pub async fn refresh_route_discovery(&self) -> Result<RouteDiscoverySnapshot, AircError> {
        self.upsert_transport_health(TransportHealthSample::healthy_direct(
            TransportKind::LocalFs,
        ))?;

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
        })
    }

    /// Snapshot the last known discovery state without mutating
    /// route health.
    pub async fn route_discovery_snapshot(&self) -> Result<RouteDiscoverySnapshot, AircError> {
        Ok(RouteDiscoverySnapshot {
            health: self.transport_health()?,
            endpoints: self.route_endpoints()?,
            connected_lan_peers: self.connected_lan_peers().await,
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
