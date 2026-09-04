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

/// Self-healing join — advertise hygiene for the LAN rung. Returns the
/// reason an IPv4 must NEVER be advertised as a dialable LAN endpoint,
/// or `None` when it is admissible.
///
/// The live M5↔bigmama repro (decay mode #4): Docker containers
/// advertised internal bridge IPs (`172.x`) as reachable endpoints and
/// every peer on the account burned dozens of 3s dial timeouts on
/// them. The rejected classes:
/// - loopback / unspecified — never dialable off-host;
/// - link-local `169.254/16` — never routed;
/// - `172.16/12` — the docker0/br-* bridge band. We detect by RANGE,
///   not interface name (the UDP source-address trick has no ifname),
///   which deliberately also refuses 172.16/12 corporate LANs: per the
///   live evidence this band is "never correct" on this mesh, and a
///   host on such a LAN still advertises its Tailscale rung;
/// - `100.64/10` CGNAT — a Tailscale address; it rides the Tailscale
///   rung ([`tailscale_advertise_rejection`]), never the LAN rung.
///
/// RFC1918 `10/8` + `192.168/16` (the en0-style LANs) and public
/// addresses pass.
pub fn lan_advertise_rejection(ip: Ipv4Addr) -> Option<&'static str> {
    let octets = ip.octets();
    if ip.is_loopback() {
        Some("loopback is never dialable off-host")
    } else if ip.is_unspecified() {
        Some("the unspecified address is not dialable")
    } else if ip.is_link_local() {
        Some("link-local (169.254/16) is never routed off-link")
    } else if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        Some(
            "172.16/12 is the docker/bridge band — advertising it floods every peer \
             with undialable ghosts (the 172.x swarm); advertise the HOST's routable \
             address instead (AIRC_ADVERTISE_IP)",
        )
    } else if is_tailscale_ipv4(ip) {
        Some("100.64/10 is a Tailscale/CGNAT address — it belongs on the Tailscale rung")
    } else {
        None
    }
}

/// Self-healing join — advertise hygiene for the Tailscale rung: the
/// address must actually BE a Tailscale/CGNAT address (`100.64/10`).
/// Anything else on this rung is a corrupted or mislabelled
/// advertisement.
pub fn tailscale_advertise_rejection(ip: Ipv4Addr) -> Option<&'static str> {
    if is_tailscale_ipv4(ip) {
        None
    } else {
        Some("not a 100.64/10 CGNAT address — the Tailscale rung only carries Tailscale IPs")
    }
}

/// Tailscale's CGNAT range is `100.64.0.0/10` (100.64.0.0 –
/// 100.127.255.255). ONE definition — the CLI's detection and the
/// advertise hygiene both use it.
pub fn is_tailscale_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

/// Apply advertise hygiene + the known-peer-collision guard to one
/// candidate rung. Returns the IP iff it may be advertised; refusals
/// are LOUD (a reachability-affecting decision is never silent).
///
/// The collision guard is decay mode #3 from the live repro: a peer
/// record carried the reader's OWN Tailscale IP as another machine's
/// endpoint. Advertising an endpoint that equals a KNOWN other peer's
/// stored endpoint would extend exactly that corruption chain onto the
/// rendezvous, so we refuse and say why.
fn admissible_advertise_ip(
    rung: &str,
    ip: Option<Ipv4Addr>,
    port: u16,
    known_peer_addrs: &std::collections::HashSet<SocketAddr>,
    rejection: fn(Ipv4Addr) -> Option<&'static str>,
) -> Option<Ipv4Addr> {
    let ip = ip?;
    if let Some(reason) = rejection(ip) {
        eprintln!("airc: refusing to advertise {ip}:{port} on the {rung} rung — {reason}");
        return None;
    }
    if known_peer_addrs.contains(&SocketAddr::from((ip, port))) {
        eprintln!(
            "airc: refusing to advertise {ip}:{port} on the {rung} rung — it equals a KNOWN \
             other peer's stored endpoint (corrupted advertisement chain; not re-poisoning \
             the rendezvous)"
        );
        return None;
    }
    Some(ip)
}

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
        let actual = self
            .bind_preferred_or_ephemeral(&adapter, preferred)
            .await?;
        self.ensure_lan_subscriber().await?;
        self.upsert_transport_health(TransportHealthSample::healthy_direct(TransportKind::LanTcp))?;
        let port = actual.port();
        // Self-healing join — advertise hygiene: never let a bridge /
        // loopback / mislabelled / peer-colliding address onto the
        // rendezvous (see `admissible_advertise_ip`).
        let known = self.known_other_peer_addrs().await?;
        let lan_ip = admissible_advertise_ip("LAN", lan_ip, port, &known, lan_advertise_rejection);
        let tailscale_ip = admissible_advertise_ip(
            "Tailscale",
            tailscale_ip,
            port,
            &known,
            tailscale_advertise_rejection,
        );
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

    /// Bind the identity-derived port, RETRYING across a restart handover,
    /// and never demote to an ephemeral port in silence.
    ///
    /// The stable port is the whole mechanism behind peers' cached endpoints
    /// surviving a restart (#8). It had a correct derivation, a passing unit
    /// test asserting "same identity must derive the same port across
    /// restarts", and a bind site that tried it first — and the node still
    /// churned its port on every restart, because of the two lines that
    /// handled failure:
    ///
    /// ```ignore
    /// Err(_preferred_taken) => adapter.listen((UNSPECIFIED, 0)).await
    /// ```
    ///
    /// Measured on the Windows node 2026-08-15, immediately after an
    /// `airc update`: peer_id `e85a5bb3-…` derives port 61539, the daemon
    /// was advertising 64463, and 61539 was BINDABLE at that moment — free,
    /// not excluded, nothing listening. The new daemon had raced the
    /// outgoing one during the update handover, lost, taken an ephemeral
    /// port, and then kept it for the whole process lifetime even after the
    /// stable port freed milliseconds later.
    ///
    /// Three defects in one expression, all of which this fixes:
    ///
    /// 1. NO RETRY. The contended window is the restart handover itself —
    ///    the single most common moment this code runs. One attempt loses it.
    /// 2. SILENT. `_preferred_taken` discards the reason, so a node that has
    ///    just become unreachable at every address its peers have cached
    ///    reports nothing at all. That is the masking fallback this repo
    ///    denies at the clippy gate, written in longhand.
    /// 3. NO RECOVERY. Once ephemeral, always ephemeral for that process.
    ///
    /// This retries briefly, and if it still cannot get the stable port it
    /// takes an ephemeral one — but says so LOUDLY and names the
    /// consequence, because "reachable at an address nobody has" is exactly
    /// the failure that reads as a quiet room rather than a broken wire.
    async fn bind_preferred_or_ephemeral(
        &self,
        adapter: &LanTcpAdapter,
        preferred: u16,
    ) -> Result<SocketAddr, AircError> {
        // Short and bounded: this covers an outgoing daemon releasing its
        // listener, which is a sub-second handover. It is deliberately not a
        // long wait — a genuinely occupied port must surface fast rather
        // than stall startup.
        const ATTEMPTS: u32 = 5;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

        let mut last_error = None;
        for attempt in 1..=ATTEMPTS {
            match adapter
                .listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, preferred)))
                .await
            {
                Ok(addr) => return Ok(addr),
                Err(error) => {
                    last_error = Some(error.to_string());
                    if attempt < ATTEMPTS {
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }
        }

        let addr = adapter
            .listen(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;

        tracing::warn!(
            preferred_port = preferred,
            fallback_port = addr.port(),
            attempts = ATTEMPTS,
            last_error = last_error.as_deref().unwrap_or("unknown"),
            "LAN bind fell back to an EPHEMERAL port — this node is now              unreachable at the identity-derived port every peer has cached              for it, and will stay on the ephemeral port until restarted.              Peers must rediscover it through the rendezvous before any              frame can cross."
        );
        Ok(addr)
    }

    /// Start the LAN presence beacon — gh-free same-network discovery.
    ///
    /// The transport crate owns the sockets and the wire format
    /// ([`airc_transport::lan_presence`]); this method owns what a hint
    /// MEANS: only an ENROLLED peer's beacon teaches anything (discovery is
    /// not authorization), and what it teaches goes into `learned_ips` — the
    /// SAME map learn-live-address (#9) feeds — so the existing dial rungs
    /// (learned-IP × advertised ports, identity-derived port) pick it up on
    /// the next route refresh with no new plumbing. A node coming online on
    /// the LAN is therefore dialable within one beacon interval plus one
    /// refresh tick, with ZERO GitHub requests spent.
    ///
    /// Logging is edge-triggered: a hint logs only when it CHANGES what we
    /// knew. At one beacon per peer per 15s, logging every packet would bury
    /// the signal it exists to provide (the bounded-window eviction class).
    pub fn start_lan_presence(&self, advertised_port: u16) -> Result<(), AircError> {
        let self_peer = self.inner.identity.peer_id;
        let learned = self.inner.learned_ips.clone();
        let registry = self.inner.registry.clone();
        airc_transport::lan_presence::spawn_lan_presence(
            self_peer.as_uuid(),
            advertised_port,
            move |hint| {
                let peer = PeerId(hint.peer_id);
                if !registry.has_peer(peer) {
                    // Unknown announcer: a public hallway is full of
                    // strangers, and none of them are routes.
                    return;
                }
                if let Ok(mut map) = learned.lock() {
                    let changed = map.get(&peer) != Some(&hint.ip);
                    if changed {
                        map.insert(peer, hint.ip);
                        tracing::info!(
                            peer = %peer,
                            ip = %hint.ip,
                            claimed_port = hint.port,
                            "LAN presence: learned peer address from multicast beacon"
                        );
                    }
                }
            },
        )
        .map_err(|error| {
            AircError::Transport(format!(
                "LAN presence bind failed — this node can still dial but cannot be                  DISCOVERED on the local network until restart: {error}"
            ))
        })
    }

    /// Every socket address stored as ANOTHER peer's dialable endpoint
    /// (both trust stores, same merge scope as the dialer). Input to the
    /// advertise collision guard — we must never advertise an address the
    /// mesh already attributes to someone else. A record whose endpoint
    /// JSON this binary can't decode contributes nothing here (the dial
    /// path already surfaces that skew loudly).
    async fn known_other_peer_addrs(
        &self,
    ) -> Result<std::collections::HashSet<SocketAddr>, AircError> {
        let mut stored = Vec::new();
        if self.inner.wire_root != self.inner.home {
            stored.extend(
                airc_trust::load(&self.inner.wire_root)
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))?,
            );
        }
        stored.extend(
            airc_trust::load(&self.inner.home)
                .await
                .map_err(|error| AircError::Transport(error.to_string()))?,
        );
        let mut addrs = std::collections::HashSet::new();
        for peer in stored {
            if peer.peer_id == self.inner.identity.peer_id {
                continue;
            }
            let Some(json) = peer.endpoints_json.as_deref() else {
                continue;
            };
            let Ok(endpoints) = crate::route::endpoints_from_json(json) else {
                continue;
            };
            for endpoint in endpoints {
                if let RouteEndpoint::LanTcp { addr } | RouteEndpoint::TailscaleTcp { addr } =
                    endpoint
                {
                    addrs.insert(addr);
                }
            }
        }
        Ok(addrs)
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

        // Self-healing join — advertise hygiene, same guard as
        // `listen_lan_advertising`. A rejected IP reads as `None`
        // below, so an already-advertised poisoned address (a bridge
        // IP, or a known peer's address) is WITHDRAWN on the next tick
        // — the poisoned-advertisement self-heal.
        let known = self.known_other_peer_addrs().await?;
        let lan_ip = admissible_advertise_ip("LAN", lan_ip, port, &known, lan_advertise_rejection);
        let tailscale_ip = admissible_advertise_ip(
            "Tailscale",
            tailscale_ip,
            port,
            &known,
            tailscale_advertise_rejection,
        );

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
        // #240 event-driven heal: forward every session termination to the
        // `on_disconnect` SLOT. Wired once here; it reads the slot each drop, so
        // `set_disconnect_observer` works before OR after this adapter is built.
        let on_disconnect = self.inner.on_disconnect.clone();
        adapter.set_disconnect_observer(std::sync::Arc::new(move |peer_id| {
            let cb = on_disconnect.lock().ok().and_then(|guard| guard.clone());
            if let Some(cb) = cb {
                cb(peer_id);
            }
        }));
        *guard = Some(adapter.clone());
        Ok(adapter)
    }

    /// #240 event-driven heal: register a callback invoked with the `peer_id`
    /// of every peer whose live LAN session terminates. The daemon uses this to
    /// nudge its route-refresh loop so a dropped-but-still-reachable peer is
    /// re-dialed at once instead of up to a full refresh interval later. Stored
    /// in a slot the LAN adapter reads each disconnect, so this may be called
    /// before or after the adapter is first built. A later call replaces it.
    pub fn set_disconnect_observer(&self, observer: std::sync::Arc<dyn Fn(PeerId) + Send + Sync>) {
        if let Ok(mut guard) = self.inner.on_disconnect.lock() {
            *guard = Some(observer);
        }
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
///
/// Public since self-healing join: the `airc dial` recovery verb uses it
/// to infer WHICH enrolled peer owns a bare `host:port` (the port is
/// identity-derived, so a match identifies the peer to cert-pin).
pub fn stable_lan_port(peer_id: PeerId) -> u16 {
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

    /// what this catches: the RESTART HANDOVER RACE that silently demoted a
    /// node to an ephemeral port — and kept it there.
    ///
    /// `stable_lan_port` was correct, and the sibling test above proved it
    /// derived the same port every time. The node churned its port anyway,
    /// because the bind site treated "preferred port busy" as a one-shot and
    /// fell through to `:0` while discarding the reason:
    ///
    /// ```ignore
    /// Err(_preferred_taken) => adapter.listen((UNSPECIFIED, 0)).await
    /// ```
    ///
    /// The contended moment is the restart handover itself — the outgoing
    /// daemon still holds the listener for a few hundred ms — which is the
    /// single most common moment this code runs. Measured on the Windows
    /// node 2026-08-15 right after an `airc update`: identity `e85a5bb3-…`
    /// derives 61539, the daemon advertised 64463, and 61539 was BINDABLE at
    /// that moment. It lost the race, took an ephemeral port, and held it for
    /// the process lifetime — so every endpoint peers had cached pointed at a
    /// dead port and inbound was structurally unreachable. M5 observed the
    /// other half independently: "your airc INBOUND is unreachable."
    ///
    /// This occupies the stable port, releases it mid-handover, and asserts
    /// the bind still lands on the STABLE port. Against the old one-shot code
    /// it fails with an ephemeral port, which is the regression.
    #[tokio::test]
    async fn a_busy_stable_port_is_retried_across_the_restart_handover() {
        let (_dir, airc) = test_airc().await;
        let preferred = stable_lan_port(airc.inner.identity.peer_id);

        // Stand in for the outgoing daemon still holding the listener.
        let outgoing =
            std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, preferred)))
                .expect("test must be able to hold the stable port to simulate the handover");

        // Release it partway through the retry window, as a real handover does.
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            drop(outgoing);
        });

        let advertised = airc
            .listen_lan_advertising(Some(Ipv4Addr::new(192, 168, 1, 50)), None)
            .await
            .expect("bind must succeed once the outgoing listener releases");

        let addr = lan_addr(&advertised).expect("a LAN endpoint must be advertised");
        assert_eq!(
            addr.port(),
            preferred,
            "bind fell back to an ephemeral port ({}) instead of retrying the identity-derived              port ({preferred}) across the handover — every endpoint peers have cached for this              node now points at a dead port, and the node cannot tell that it is unreachable",
            addr.port()
        );
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

    // what this catches (self-healing join, M5↔bigmama decay mode #4 —
    // the 172.x ghost swarm): the advertise-hygiene predicate must
    // refuse every never-correct class (loopback, unspecified,
    // link-local, the docker/bridge 172.16/12 band, CGNAT on the LAN
    // rung, non-CGNAT on the Tailscale rung) and admit real en0-style
    // LANs + public addresses. Mutation check: dropping any rejection
    // arm fails its assert.
    #[test]
    fn advertise_hygiene_rejects_never_correct_classes_and_admits_real_lans() {
        // Rejected on the LAN rung.
        for poisoned in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::new(169, 254, 7, 7),  // link-local
            Ipv4Addr::new(172, 16, 0, 2),   // docker/bridge band low edge
            Ipv4Addr::new(172, 18, 0, 5),   // the literal live ghost class
            Ipv4Addr::new(172, 31, 255, 9), // band high edge
            Ipv4Addr::new(100, 79, 156, 3), // Tailscale IP on the WRONG rung
        ] {
            assert!(
                lan_advertise_rejection(poisoned).is_some(),
                "{poisoned} must be refused on the LAN rung"
            );
        }
        // Admitted on the LAN rung.
        for real in [
            Ipv4Addr::new(192, 168, 1, 232), // en0-style home/office LAN
            Ipv4Addr::new(10, 0, 1, 16),     // 10/8 LAN
            Ipv4Addr::new(172, 32, 0, 1),    // just past the bridge band
            Ipv4Addr::new(8, 8, 8, 8),       // public
        ] {
            assert!(
                lan_advertise_rejection(real).is_none(),
                "{real} must be advertisable on the LAN rung"
            );
        }
        // The Tailscale rung only carries CGNAT.
        assert!(tailscale_advertise_rejection(Ipv4Addr::new(100, 79, 156, 3)).is_none());
        assert!(tailscale_advertise_rejection(Ipv4Addr::new(192, 168, 1, 232)).is_some());
    }

    // what this catches (self-healing join): an already-advertised
    // POISONED address (a docker-bridge IP that slipped onto the
    // rendezvous pre-hygiene) must be WITHDRAWN by the refresh tick,
    // not re-advertised — the rejected IP reads as None and takes the
    // existing withdraw arm. Mutation check: dropping the
    // `admissible_advertise_ip` filter in `refresh_advertised_endpoints`
    // keeps the bridge IP advertised and fails both asserts.
    #[tokio::test]
    async fn refresh_withdraws_a_poisoned_advertised_bridge_ip() {
        let (_dir, airc) = test_airc().await;
        // A pre-hygiene daemon advertised its docker bridge address.
        airc.upsert_route_endpoint(RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(172, 18, 0, 5), 7777)),
        })
        .unwrap();

        // The tick re-detects the same poisoned IP — hygiene must refuse
        // it and withdraw the rung instead of keeping the ghost alive.
        let changed = airc
            .refresh_advertised_endpoints(Some(Ipv4Addr::new(172, 18, 0, 5)), None)
            .await
            .unwrap();

        assert!(changed, "withdrawing the poisoned rung is a change");
        assert_eq!(
            lan_addr(&airc.route_endpoints().unwrap()),
            None,
            "a bridge IP must never survive on the advertised set"
        );
    }

    // what this catches (self-healing join, decay mode #3 — a peer
    // record carrying OUR address as another machine's endpoint): we
    // must never advertise an endpoint that equals a KNOWN other
    // peer's stored endpoint — that would extend the corruption chain
    // onto the rendezvous. Mutation check: dropping the
    // `known_peer_addrs.contains` guard advertises the colliding
    // address and fails the assert.
    #[tokio::test]
    async fn refresh_refuses_to_advertise_a_known_peers_address() {
        let (_dir, airc) = test_airc().await;
        // A known peer's stored endpoint at (192.168.1.50, 7777).
        let other = PeerId::from_u128(0x07e6_0000_0001);
        airc_trust::add(airc.wire_root(), other, [7u8; 32])
            .await
            .unwrap();
        let json = crate::route::endpoints_to_json(&[RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(192, 168, 1, 50), 7777)),
        }])
        .unwrap();
        airc_trust::set_endpoints_json(airc.wire_root(), other, Some(json), 1_000, None)
            .await
            .unwrap()
            .expect("other peer enrolled");

        // Our currently-advertised rung shares port 7777; the tick then
        // "detects" the other peer's IP (the corrupted-chain shape).
        airc.upsert_route_endpoint(RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(192, 168, 1, 232), 7777)),
        })
        .unwrap();
        let changed = airc
            .refresh_advertised_endpoints(Some(Ipv4Addr::new(192, 168, 1, 50)), None)
            .await
            .unwrap();

        assert!(changed, "the stale own-rung must be withdrawn");
        assert_eq!(
            lan_addr(&airc.route_endpoints().unwrap()),
            None,
            "a KNOWN other peer's address must never be advertised as ours"
        );
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
