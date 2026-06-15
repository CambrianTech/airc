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

/// Card 625abe6d slice 1 — per-dial deadline. LAN/tailnet targets
/// complete a TCP+TLS handshake well inside this on any healthy
/// path; a SYN-dropping firewall otherwise pins the OS connect for
/// ~21s (Windows) to ~130s (Linux) PER endpoint, turning
/// `transport health` / `doctor` into a multi-minute hang — and a
/// poisoned registry document could plant tarpit endpoints
/// deliberately (#1120 sentinel blocking-2).
const PEER_DIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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
        // Both registries: the machine-account wire root (where
        // account-registry import writes) FIRST, then the scope's own
        // store (where the CLI's `peer add --endpoint` writes).
        // Endpoints are MERGED per peer across both stores — #1120
        // sentinel blocking-1 proved that first-record-wins dedupe
        // lets an endpoint-LESS row from one store shadow the
        // endpoint-carrying row from the other (the import path
        // creates exactly that split), silently producing zero dials
        // on every real machine while single-store tests stay green.
        // Wire-root order first = fresh registry data dials before a
        // possibly-stale dev `--endpoint` override.
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

        let connected: std::collections::HashSet<PeerId> =
            self.connected_lan_peers().await.into_iter().collect();
        let mut failures = Vec::new();
        // Card 7e3c9a1f: one wall-clock read for the whole refresh drives
        // the dial-failure backoff (skip endpoints still inside their
        // quarantine window, stamp new failures, clear on success).
        let now_ms = crate::time::now_ms()?;

        // Merge endpoints per peer, preserving first-seen (wire-root)
        // order and dropping duplicates. A record whose endpoint JSON
        // this binary can't decode becomes a PER-PEER failure, not a
        // whole-refresh abort — one skewed record must not brick route
        // discovery for every other peer (the repo's version-skew
        // posture: tolerate what you don't understand, loudly).
        let mut order: Vec<PeerId> = Vec::new();
        let mut merged: std::collections::HashMap<PeerId, Vec<RouteEndpoint>> =
            std::collections::HashMap::new();
        for peer in stored {
            if peer.peer_id == self.inner.identity.peer_id || connected.contains(&peer.peer_id) {
                continue;
            }
            let Some(json) = peer.endpoints_json.as_deref() else {
                continue;
            };
            let endpoints = match crate::route::endpoints_from_json(json) {
                Ok(endpoints) => endpoints,
                Err(error) => {
                    failures.push(PeerDialFailure {
                        peer_id: peer.peer_id,
                        endpoint: RouteEndpoint::Relay {
                            url: "<undecodable record>".to_string(),
                        },
                        error: format!(
                            "endpoint JSON this binary cannot decode (version skew?): {error}"
                        ),
                    });
                    continue;
                }
            };
            let slot = merged.entry(peer.peer_id).or_insert_with(|| {
                order.push(peer.peer_id);
                Vec::new()
            });
            for endpoint in endpoints {
                if !slot.contains(&endpoint) {
                    slot.push(endpoint);
                }
            }
        }

        for peer_id in order {
            let Some(endpoints) = merged.get(&peer_id) else {
                continue;
            };
            for endpoint in endpoints {
                let addr = match endpoint {
                    RouteEndpoint::LanTcp { addr } | RouteEndpoint::TailscaleTcp { addr } => *addr,
                    // Relay/UDP/WebRTC/Reticulum dialing lands in later
                    // slices; recording them as failures here would be
                    // noise about unimplemented transports, not signal
                    // about unreachable peers. Variants enumerated, not
                    // wildcarded, per the production no-silent-fallback
                    // clippy gate: a FUTURE endpoint variant must force
                    // this match to be revisited, not silently fall into
                    // "skip" (the wildcard slipped past #1120's review
                    // because the merge-push CI run was hand-cancelled —
                    // caught by the #1121 sentinel).
                    RouteEndpoint::Udp { .. }
                    | RouteEndpoint::Relay { .. }
                    | RouteEndpoint::Reticulum { .. }
                    | RouteEndpoint::WebRtcSignaling { .. } => continue,
                };
                // Card 7e3c9a1f: skip endpoints still inside their
                // dial-failure backoff window. A daemon that restarted on
                // a new port leaves its old `addr` in every peer's trust
                // store until the registry re-converges; without this
                // skip each refresh re-pays PEER_DIAL_TIMEOUT on that
                // corpse, starving the dial to the live endpoint. The
                // freshest (most likely live) endpoint is listed first and
                // is never quarantined unless it itself just failed, so
                // this never blocks a genuinely reachable peer.
                if self.dial_quarantine_is_quarantined(&addr, now_ms) {
                    continue;
                }
                // #1120 sentinel blocking-2: connect_lan has no inner
                // timeout, and a SYN-dropping firewall (the default
                // posture of the NATs this card exists to cross) hangs
                // the OS connect for ~21-130s per endpoint. Bound every
                // dial; a timeout is a recorded failure like any other.
                match tokio::time::timeout(PEER_DIAL_TIMEOUT, self.connect_lan(addr, peer_id)).await
                {
                    Ok(Ok(())) => {
                        // Card 7e3c9a1f: a live connect lifts any prior
                        // quarantine so a flapped-but-recovered endpoint is
                        // immediately eligible again next refresh.
                        self.dial_quarantine_record_success(&addr);
                        break;
                    }
                    Ok(Err(error)) => {
                        self.dial_quarantine_record_failure(addr, now_ms);
                        failures.push(PeerDialFailure {
                            peer_id,
                            endpoint: endpoint.clone(),
                            error: error.to_string(),
                        });
                    }
                    Err(_elapsed) => {
                        self.dial_quarantine_record_failure(addr, now_ms);
                        failures.push(PeerDialFailure {
                            peer_id,
                            endpoint: endpoint.clone(),
                            error: format!(
                                "dial timed out after {}s (endpoint unreachable or \
                                 firewall drops SYN)",
                                PEER_DIAL_TIMEOUT.as_secs()
                            ),
                        });
                    }
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

    /// Card 7e3c9a1f: is `addr` still inside its dial-failure backoff
    /// window? Brief lock, never held across an await.
    fn dial_quarantine_is_quarantined(&self, addr: &std::net::SocketAddr, now_ms: u64) -> bool {
        let guard = self
            .inner
            .dial_quarantine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.is_quarantined(addr, now_ms)
    }

    /// Card 7e3c9a1f: stamp a failed dial to `addr` (starts/doubles the
    /// backoff).
    fn dial_quarantine_record_failure(&self, addr: std::net::SocketAddr, now_ms: u64) {
        let mut guard = self
            .inner
            .dial_quarantine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.record_failure(addr, now_ms);
    }

    /// Card 7e3c9a1f: clear any quarantine on `addr` after a live connect.
    fn dial_quarantine_record_success(&self, addr: &std::net::SocketAddr) {
        let mut guard = self
            .inner
            .dial_quarantine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.record_success(addr);
    }

    async fn connected_lan_peers(&self) -> Vec<PeerId> {
        let adapter = self.inner.lan_tcp.lock().await.clone();
        match adapter {
            Some(adapter) => adapter.connected_peers().await,
            None => Vec::new(),
        }
    }
}
