//! LAN transport binding for embedded AIRC handles.
//!
//! LAN is a substrate transport concern. Consumer apps call these SDK
//! methods; they do not own socket setup, adapter state, route health,
//! or frame ingestion.

use std::net::{Ipv4Addr, SocketAddr};

use airc_core::PeerId;
use airc_transport::LanTcpAdapter;

use crate::error::AircError;
use crate::route::{RouteEndpoint, TransportHealthSample, TransportKind};
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
        self.upsert_transport_health(TransportHealthSample::healthy_direct(TransportKind::LanTcp))?;
        self.upsert_route_endpoint(RouteEndpoint::LanTcp { addr: actual })?;
        Ok(actual)
    }

    /// Bind ONE all-interfaces LAN listener and advertise it under every
    /// dialable address this host owns (its LAN IP and/or its Tailscale
    /// IP). The adapter supports a single bound listener, so we bind
    /// `0.0.0.0:0` once — a wildcard socket accepts on EVERY interface,
    /// meaning the same port is reachable via the `192.168.x` LAN address
    /// AND the `100.x` Tailscale address. We then publish BOTH endpoints.
    ///
    /// This realizes the connection ladder (lowest common denominator
    /// first): a same-subnet peer dials the LAN address directly — no
    /// Tailscale hop — and only a cross-network / firewalled peer falls
    /// through to the `100.x` address that traverses NAT. The dialer
    /// already tries endpoints in `RouteEndpointKind` order (LanTcp before
    /// TailscaleTcp) and breaks on first success, so advertising both is
    /// all that's needed for "Tailscale only if we leave the LAN".
    ///
    /// Returns the endpoints actually advertised (for the caller to log).
    /// Unlike [`Airc::listen_lan`], this never advertises the wildcard
    /// `0.0.0.0` bind address — peers receive only dialable specific IPs.
    pub async fn listen_lan_advertising(
        &self,
        lan_ip: Option<Ipv4Addr>,
        tailscale_ip: Option<Ipv4Addr>,
    ) -> Result<Vec<RouteEndpoint>, AircError> {
        let adapter = self.lan_adapter().await?;
        let actual = adapter
            .listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        self.ensure_lan_subscriber().await?;
        self.upsert_transport_health(TransportHealthSample::healthy_direct(TransportKind::LanTcp))?;
        let port = actual.port();
        let mut advertised = Vec::new();
        if let Some(ip) = lan_ip {
            let endpoint = RouteEndpoint::LanTcp {
                addr: SocketAddr::from((ip, port)),
            };
            self.upsert_route_endpoint(endpoint.clone())?;
            advertised.push(endpoint);
        }
        if let Some(ip) = tailscale_ip {
            let endpoint = RouteEndpoint::TailscaleTcp {
                addr: SocketAddr::from((ip, port)),
            };
            self.upsert_route_endpoint(endpoint.clone())?;
            advertised.push(endpoint);
        }
        Ok(advertised)
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
        self.upsert_transport_health(TransportHealthSample::healthy_direct(TransportKind::LanTcp))?;
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
