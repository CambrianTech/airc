//! Relay transport binding for embedded AIRC handles.
//!
//! Relay is the cross-boundary live route for peers that cannot reach
//! each other directly. The SDK owns adapter lifecycle, route health,
//! and frame ingestion; consumers only provide the relay endpoint and
//! pinned relay identity.

use std::net::SocketAddr;

use airc_core::PeerId;
use airc_transport::relay::{RelayAdapter, RelayClientConfig};

use crate::error::AircError;
use crate::route::{
    RouteEndpoint, TransportHealthSample, TransportHealthState, TransportKind, TransportRole,
};
use crate::Airc;

impl Airc {
    /// Connect this handle to a pinned relay and make the relay route
    /// available for subsequent sends when route health selects it.
    pub async fn connect_relay(
        &self,
        relay_addr: SocketAddr,
        relay_peer: PeerId,
    ) -> Result<(), AircError> {
        {
            let guard = self.inner.relay.lock().await;
            if guard.is_some() {
                drop(guard);
                self.ensure_relay_subscriber().await?;
                self.upsert_relay_health(relay_addr)?;
                return Ok(());
            }
        }

        let adapter = RelayAdapter::new(RelayClientConfig {
            self_peer_id: self.inner.identity.peer_id,
            self_keypair: self.inner.identity.keypair.clone(),
            relay_peer_id: relay_peer,
            relay_addr,
            registry: self.inner.registry.clone(),
        });
        adapter
            .connect()
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        {
            let mut guard = self.inner.relay.lock().await;
            if guard.is_none() {
                *guard = Some(adapter.clone());
            }
        }
        self.ensure_relay_subscriber().await?;
        self.upsert_relay_health(relay_addr)?;
        Ok(())
    }

    fn upsert_relay_health(&self, relay_addr: SocketAddr) -> Result<(), AircError> {
        self.upsert_transport_health(TransportHealthSample {
            kind: TransportKind::Relay,
            role: TransportRole::Relay,
            state: TransportHealthState::Healthy,
            rtt_ms: None,
            success_ppm: None,
        })?;
        self.upsert_route_endpoint(RouteEndpoint::Relay {
            url: format!("airc-relay://{relay_addr}"),
        })?;
        Ok(())
    }

    pub(crate) async fn relay_adapter(&self) -> Result<RelayAdapter, AircError> {
        let guard = self.inner.relay.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AircError::Transport("relay adapter is not connected".into()))
    }
}
