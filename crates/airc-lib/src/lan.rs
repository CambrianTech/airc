//! LAN transport binding for embedded AIRC handles.
//!
//! LAN is a substrate transport concern. Consumer apps call these SDK
//! methods; they do not own socket setup, adapter state, route health,
//! or frame ingestion.

use std::net::SocketAddr;

use airc_core::PeerId;
use airc_transport::LanTcpAdapter;

use crate::error::AircError;
use crate::route_health::TransportHealthSample;
use crate::route_policy::TransportKind;
use crate::Airc;

impl Airc {
    /// Bind a TLS-pinned LAN listener and ingest received frames into
    /// the same store/live stream as local-fs frames.
    pub async fn listen_lan(&self, bind: SocketAddr) -> Result<SocketAddr, AircError> {
        let adapter = self.lan_adapter().await?;
        let actual = adapter
            .listen(bind)
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        self.ensure_lan_subscriber().await?;
        self.replace_transport_health([TransportHealthSample::healthy_direct(
            TransportKind::LanTcp,
        )])?;
        Ok(actual)
    }

    /// Connect to a TLS-pinned LAN peer and make LAN-TCP the active
    /// direct route for subsequent sends on this handle.
    pub async fn connect_lan(
        &self,
        peer_addr: SocketAddr,
        expected_peer: PeerId,
    ) -> Result<(), AircError> {
        let adapter = self.lan_adapter().await?;
        adapter
            .connect(peer_addr, expected_peer)
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        self.ensure_lan_subscriber().await?;
        self.replace_transport_health([TransportHealthSample::healthy_direct(
            TransportKind::LanTcp,
        )])?;
        Ok(())
    }

    pub(crate) async fn lan_adapter(&self) -> Result<LanTcpAdapter, AircError> {
        let mut guard = self.inner.lan_tcp.lock().await;
        if let Some(adapter) = guard.as_ref() {
            return Ok(adapter.clone());
        }
        let adapter = LanTcpAdapter::new(
            self.inner.identity.peer_id,
            self.inner.identity.keypair.clone(),
            self.inner.registry.clone(),
        )
        .map_err(|error| AircError::Transport(error.to_string()))?;
        *guard = Some(adapter.clone());
        Ok(adapter)
    }
}
