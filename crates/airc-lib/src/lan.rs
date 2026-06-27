//! LAN transport binding for embedded AIRC handles.
//!
//! LAN is a substrate transport concern. Consumer apps call these SDK
//! methods; they do not own socket setup, adapter state, route health,
//! or frame ingestion.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

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
        // Bind a STABLE port derived from our identity so the advertised
        // endpoint survives daemon restarts. An ephemeral `:0` re-rolls the
        // port every restart, staling every peer's stored endpoint for us —
        // the root of the cross-machine auto-connect churn (#8). Fall back to
        // an OS-assigned port only if the preferred one is already taken.
        let preferred = stable_lan_port(self.inner.identity.peer_id);
        let actual = match adapter
            .listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, preferred)))
            .await
        {
            Ok(addr) => addr,
            Err(_preferred_taken) => adapter
                .listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
                .await
                .map_err(|error| AircError::Transport(error.to_string()))?,
        };
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

    /// Re-evaluate the dialable endpoints this daemon advertises against
    /// the CURRENTLY detected LAN / Tailscale IPv4, and apply any change
    /// in place — the self-heal for a roaming / router-swap / DHCP-renew /
    /// Tailscale-toggle network change. Without this the endpoint computed
    /// once at daemon start is frozen, so a node that changes IP keeps
    /// advertising a stale, undialable address until it is manually
    /// restarted (the bug this fixes).
    ///
    /// The wildcard `0.0.0.0` listener bound by [`Self::listen_lan_advertising`]
    /// accepts on whatever interfaces the host currently has, so an IP
    /// change needs NO rebind — rebinding would sever live connections
    /// (the adapter holds ONE listener for accept + dial + forward). Only
    /// the ADVERTISED address must follow the new IP, so we reuse the
    /// already-bound port and upsert / withdraw the `LanTcp` /
    /// `TailscaleTcp` endpoints to match `lan_ip` / `tailscale_ip`.
    ///
    /// Returns `true` iff the advertised set changed — the caller resyncs
    /// the account-registry card ONLY then (no change ⇒ no gist write ⇒ no
    /// spam). Edge case: if no listener is bound yet (the daemon started
    /// with no routable IP) and an IP has since appeared, this binds once
    /// via [`Self::listen_lan_advertising`].
    pub async fn refresh_advertised_endpoints(
        &self,
        lan_ip: Option<Ipv4Addr>,
        tailscale_ip: Option<Ipv4Addr>,
    ) -> Result<bool, AircError> {
        let current = self.route_endpoints()?;
        // `if let` (not a `_ =>` match) so the production no-silent-fallback
        // gate's `wildcard_enum_match_arm` deny stays satisfied without
        // enumerating every other RouteEndpoint variant.
        let current_lan = current.iter().find_map(|endpoint| {
            if let RouteEndpoint::LanTcp { addr } = endpoint {
                Some(*addr)
            } else {
                None
            }
        });
        let current_tailscale = current.iter().find_map(|endpoint| {
            if let RouteEndpoint::TailscaleTcp { addr } = endpoint {
                Some(*addr)
            } else {
                None
            }
        });

        // The one wildcard listener's port is shared by both rungs. If
        // neither rung is advertised, no listener is bound yet.
        let Some(port) = current_lan.or(current_tailscale).map(|addr| addr.port()) else {
            if lan_ip.is_some() || tailscale_ip.is_some() {
                let advertised = self.listen_lan_advertising(lan_ip, tailscale_ip).await?;
                return Ok(!advertised.is_empty());
            }
            return Ok(false);
        };

        let mut changed = false;

        // LAN rung: upsert on IP change/appearance, withdraw on disappearance.
        match (lan_ip, current_lan) {
            (Some(ip), current) if current.map(|addr| addr.ip()) != Some(IpAddr::V4(ip)) => {
                self.upsert_route_endpoint(RouteEndpoint::LanTcp {
                    addr: SocketAddr::from((ip, port)),
                })?;
                changed = true;
            }
            (None, Some(_)) => {
                self.inner.route_endpoints.remove_lan();
                changed = true;
            }
            _ => {}
        }

        // Tailscale rung: same diff, so toggling Tailscale on/off heals too.
        match (tailscale_ip, current_tailscale) {
            (Some(ip), current) if current.map(|addr| addr.ip()) != Some(IpAddr::V4(ip)) => {
                self.upsert_route_endpoint(RouteEndpoint::TailscaleTcp {
                    addr: SocketAddr::from((ip, port)),
                })?;
                changed = true;
            }
            (None, Some(_)) => {
                self.inner.route_endpoints.remove_tailscale();
                changed = true;
            }
            _ => {}
        }

        Ok(changed)
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
        // #9: learn each authenticated inbound peer's real IP. A peer that
        // dialed us proved it's reachable at that source IP; the dial path
        // pairs it with the peer's stable advertised port so a peer whose
        // published endpoint went stale is still dialable. Registered once,
        // on first adapter creation.
        let learned_ips = self.inner.learned_ips.clone();
        adapter.set_inbound_observer(std::sync::Arc::new(move |peer_id, ip| {
            if let Ok(mut map) = learned_ips.lock() {
                map.insert(peer_id, ip);
            }
        }));
        *guard = Some(adapter.clone());
        Ok(adapter)
    }
}

/// A stable LAN listener port derived from this peer's identity (#8).
///
/// An ephemeral `0.0.0.0:0` bind re-rolls the port on every daemon restart,
/// so every peer's STORED endpoint for this node goes stale the moment it
/// restarts — the root cause of the cross-machine auto-connect churn (a peer
/// keeps dialing the dead old port until the registry re-converges). Deriving
/// the port from the (stable, persisted) peer_id makes the advertised
/// endpoint survive restarts. The range is the IANA dynamic/private band
/// (49152..=65535); two scopes on one machine have different peer_ids →
/// different ports (no self-collision), and the caller falls back to an
/// ephemeral port if this one is already taken.
fn stable_lan_port(peer_id: PeerId) -> u16 {
    const DYNAMIC_BASE: u128 = 49152; // first IANA dynamic/private port
    const DYNAMIC_SPAN: u128 = 65536 - DYNAMIC_BASE; // 16384 ports
    (DYNAMIC_BASE + (peer_id.as_uuid().as_u128() % DYNAMIC_SPAN)) as u16
}

#[cfg(test)]
mod tests {
    //! Network-change self-heal: `refresh_advertised_endpoints` must
    //! follow this node's LAN/Tailscale IP without a rebind, and report a
    //! change ONLY when the advertised set actually moved (so the caller
    //! resyncs the gist card edge-triggered, never on every tick).
    use super::*;
    use crate::route::RouteEndpoint;
    use tempfile::tempdir;

    // what this catches: the ephemeral-port churn regression. The advertised
    // port MUST be stable across restarts (same identity → same port) and in
    // the IANA dynamic range; two identities must (almost always) differ so
    // co-located scopes don't collide.
    #[test]
    fn stable_lan_port_is_deterministic_per_identity_and_in_range() {
        let a = PeerId::from_u128(0x550e8400_e29b_41d4_a716_446655440000);
        let b = PeerId::from_u128(0x111e8400_e29b_41d4_a716_4466554400ff);
        assert_eq!(
            stable_lan_port(a),
            stable_lan_port(a),
            "same identity must derive the same port across restarts"
        );
        assert_ne!(
            stable_lan_port(a),
            stable_lan_port(b),
            "distinct identities should derive distinct ports (no co-located self-collision)"
        );
        for id in [a, b] {
            assert!(
                (49152..=65535).contains(&stable_lan_port(id)),
                "port must be in the IANA dynamic/private range"
            );
        }
    }

    async fn test_airc() -> (tempfile::TempDir, Airc) {
        let dir = tempdir().unwrap();
        let airc = Airc::open_with_wire_root_for_test(
            dir.path().join("machine/.airc"),
            dir.path().join("wire"),
        )
        .await
        .unwrap();
        (dir, airc)
    }

    fn lan_addr(endpoints: &[RouteEndpoint]) -> Option<SocketAddr> {
        endpoints.iter().find_map(|e| match e {
            RouteEndpoint::LanTcp { addr } => Some(*addr),
            _ => None,
        })
    }
    fn tailscale_addr(endpoints: &[RouteEndpoint]) -> Option<SocketAddr> {
        endpoints.iter().find_map(|e| match e {
            RouteEndpoint::TailscaleTcp { addr } => Some(*addr),
            _ => None,
        })
    }

    // what this catches: the frozen-endpoint bug — a node whose LAN IP
    // moved kept advertising the stale address. The new IP must replace
    // the old on the SAME port, and the call must report `changed`.
    #[tokio::test]
    async fn lan_ip_change_readvertises_same_port_and_reports_changed() {
        let (_dir, airc) = test_airc().await;
        // Seed an already-advertised LAN endpoint on a known port.
        airc.upsert_route_endpoint(RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(10, 0, 1, 16), 7777)),
        })
        .unwrap();

        let changed = airc
            .refresh_advertised_endpoints(Some(Ipv4Addr::new(192, 168, 1, 232)), None)
            .await
            .unwrap();

        assert!(changed, "an IP move must be reported as a change");
        let endpoints = airc.route_endpoints().unwrap();
        assert_eq!(
            lan_addr(&endpoints),
            Some(SocketAddr::from((Ipv4Addr::new(192, 168, 1, 232), 7777))),
            "new IP must be advertised on the SAME bound port (no rebind)"
        );
    }

    // what this catches: spam. A tick where nothing moved must NOT report
    // a change, or the registry loop would rewrite the gist every tick.
    #[tokio::test]
    async fn unchanged_ip_reports_no_change() {
        let (_dir, airc) = test_airc().await;
        airc.upsert_route_endpoint(RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(10, 0, 1, 16), 7777)),
        })
        .unwrap();

        let changed = airc
            .refresh_advertised_endpoints(Some(Ipv4Addr::new(10, 0, 1, 16)), None)
            .await
            .unwrap();
        assert!(!changed, "same IP ⇒ no change ⇒ no resync (no spam)");
    }

    // what this catches: Tailscale toggle. Turning Tailscale on adds the
    // rung on the same port; turning it off withdraws it — both reported.
    #[tokio::test]
    async fn tailscale_toggle_adds_then_withdraws() {
        let (_dir, airc) = test_airc().await;
        airc.upsert_route_endpoint(RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(192, 168, 1, 232), 7777)),
        })
        .unwrap();

        // Tailscale comes up.
        let changed = airc
            .refresh_advertised_endpoints(
                Some(Ipv4Addr::new(192, 168, 1, 232)),
                Some(Ipv4Addr::new(100, 79, 156, 3)),
            )
            .await
            .unwrap();
        assert!(changed);
        let endpoints = airc.route_endpoints().unwrap();
        assert_eq!(
            tailscale_addr(&endpoints),
            Some(SocketAddr::from((Ipv4Addr::new(100, 79, 156, 3), 7777))),
            "Tailscale rung shares the one wildcard port"
        );

        // Tailscale goes away → withdrawn.
        let changed = airc
            .refresh_advertised_endpoints(Some(Ipv4Addr::new(192, 168, 1, 232)), None)
            .await
            .unwrap();
        assert!(changed);
        assert_eq!(
            tailscale_addr(&airc.route_endpoints().unwrap()),
            None,
            "Tailscale off must withdraw the stale rung"
        );
    }
}
