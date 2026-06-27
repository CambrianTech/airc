//! UDP transport binding for embedded AIRC handles.
//!
//! UDP is the latency-priority datagram route — pose streams, fast
//! game state, anything where head-of-line blocking under packet loss
//! is worse than the loss itself. The SDK owns adapter lifecycle and
//! frame ingestion; callers provide the bind address and the known
//! peer-id → socket-addr table.
//!
//! Unlike LAN-TCP, UDP has no connection handshake — `bind_udp`
//! returns once the socket is open and known peers are registered.
//! Sends are immediate.

use std::collections::HashMap;
use std::net::SocketAddr;

use airc_core::PeerId;
use airc_transport::udp::{UdpAdapter, UdpConfig};

use crate::error::AircError;
use crate::route::{
    RouteEndpoint, TransportHealthSample, TransportHealthState, TransportKind, TransportRole,
};
use crate::Airc;

impl Airc {
    /// Bind a UDP socket on `local_addr` and register the given peer
    /// endpoints. Returns the actual bound address (resolves
    /// `port = 0` to the OS-assigned ephemeral port so callers can
    /// share their address with peers out-of-band).
    ///
    /// Idempotent: a second call on an already-bound handle is a
    /// no-op that simply refreshes the route-health entry and
    /// returns the existing bound address.
    pub async fn bind_udp(
        &self,
        local_addr: SocketAddr,
        peer_endpoints: HashMap<PeerId, SocketAddr>,
    ) -> Result<SocketAddr, AircError> {
        {
            let guard = self.inner.udp.lock().await;
            if let Some(adapter) = guard.as_ref() {
                let bound = adapter
                    .local_addr()
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))?;
                drop(guard);
                self.ensure_udp_subscriber().await?;
                self.upsert_udp_health(bound)?;
                return Ok(bound);
            }
        }

        let mut config = UdpConfig::new(local_addr);
        for (peer, addr) in peer_endpoints {
            config = config.with_peer(peer, addr);
        }
        let adapter = UdpAdapter::new(config);
        let bound = adapter
            .bind()
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;

        {
            let mut guard = self.inner.udp.lock().await;
            if guard.is_none() {
                *guard = Some(adapter);
            }
        }
        self.ensure_udp_subscriber().await?;
        self.upsert_udp_health(bound)?;
        Ok(bound)
    }

    fn upsert_udp_health(&self, bound: SocketAddr) -> Result<(), AircError> {
        self.upsert_transport_health(TransportHealthSample {
            kind: TransportKind::Udp,
            role: TransportRole::Direct,
            state: TransportHealthState::Healthy,
            rtt_ms: None,
            success_ppm: None,
        })?;
        self.upsert_route_endpoint(RouteEndpoint::Udp { addr: bound })?;
        Ok(())
    }

    pub(crate) async fn udp_adapter(&self) -> Result<UdpAdapter, AircError> {
        let guard = self.inner.udp.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AircError::Transport("udp adapter is not bound".into()))
    }

    /// Register `peer_id`'s UDP endpoint at runtime. Required when the
    /// peer's address is learned after `bind_udp` was called (e.g.
    /// signalling, gossip, discovery). Returns the previous endpoint
    /// if the peer was already registered.
    pub async fn add_udp_peer(
        &self,
        peer_id: PeerId,
        addr: SocketAddr,
    ) -> Result<Option<SocketAddr>, AircError> {
        let adapter = self.udp_adapter().await?;
        Ok(adapter.add_peer(peer_id, addr))
    }
}
