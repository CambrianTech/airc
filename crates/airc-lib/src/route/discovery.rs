//! Route discovery and health ingestion.
//!
//! Discovery reads substrate-owned state and turns it into route
//! health/endpoints. Consumers should not hand-edit route health just
//! to make normal local/LAN operation work.

use airc_core::PeerId;
use futures::stream::StreamExt;

use crate::error::AircError;
use crate::route::health::{TransportHealthSample, TransportHealthState};
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

/// Max peers dialed CONCURRENTLY in one route refresh. Per-peer dials
/// are independent, so the refresh wall-clock is `~max(single-peer
/// cost)` rather than the SUM of every unreachable peer's
/// `PEER_DIAL_TIMEOUT` — the serial sum hung `doctor --health` (and
/// any send-path refresh) for tens of seconds on a real account with
/// dozens of enrolled peers (41 on BIGMAMA: ~15 offline × 3s ≈ 45s).
/// Bounded so a large grid does not open hundreds of sockets at once.
const DIAL_CONCURRENCY: usize = 16;

/// One peer's route-dial result: the endpoints that failed to dial and
/// the endpoints skipped for being in backoff. Named so the concurrent
/// collection below doesn't trip clippy's `type_complexity` gate.
type PeerDialOutcome = (Vec<PeerDialFailure>, Vec<PeerDialSkip>);

/// #10: is this enrolled peer a GHOST — no fresh contact within the freshness
/// TTL? A live peer's registry beacon refreshes `last_seen_ms` every cycle, so
/// a peer past the TTL is a dead container / abandoned scope whose stale
/// endpoint we should NOT keep dialing. Uses the same TTL as the registry's
/// liveness definition (one source of truth) and saturating-subs so a future
/// `last_seen` (clock skew, already clamped on import) reads as fresh, never
/// underflows.
fn is_ghost_peer(now_ms: u64, last_seen_ms: u64) -> bool {
    now_ms.saturating_sub(last_seen_ms) > crate::account_registry::DEFAULT_PEER_FRESHNESS_TTL_MS
}

/// #9: build the LEARNED dial candidates for a peer — its known-reachable
/// `learned_ip` paired with each STABLE port the peer already advertises (#8).
/// A peer that connected to us proved it's reachable at `learned_ip`, so even
/// if its PUBLISHED endpoint's IP went stale, `(learned_ip, advertised_port)`
/// is its real listener. Returns only NEW candidates (not already in
/// `existing`), de-duplicated, so a learned IP that already matches a stored
/// endpoint adds nothing.
fn learned_lan_candidates(
    existing: &[RouteEndpoint],
    learned_ip: std::net::IpAddr,
) -> Vec<RouteEndpoint> {
    let mut candidates = Vec::new();
    for endpoint in existing {
        // The peer's advertised port (LAN or Tailscale rung — same wildcard
        // listener under #8). `if let` (not `_ =>`) keeps the no-silent-fallback
        // gate's wildcard_enum_match_arm satisfied.
        if let RouteEndpoint::LanTcp { addr } | RouteEndpoint::TailscaleTcp { addr } = endpoint {
            let candidate = RouteEndpoint::LanTcp {
                addr: std::net::SocketAddr::from((learned_ip, addr.port())),
            };
            if !existing.contains(&candidate) && !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

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

/// Card 7e3c9a1f — a stored peer endpoint that was NOT dialed this
/// refresh because it is still inside its dial-failure backoff window.
/// Distinct from [`PeerDialFailure`]: NO dial was attempted, so this must
/// NOT be reported as "a dial failed" (which would emit false
/// `PeerDialFailed` warnings every refresh and inflate the failure count).
/// Surfaced as its own channel so `airc transport health` can show "in
/// backoff" honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDialSkip {
    pub peer_id: PeerId,
    pub endpoint: RouteEndpoint,
    /// Milliseconds of backoff remaining before this endpoint is dialed
    /// again.
    pub remaining_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDiscoverySnapshot {
    pub health: Vec<TransportHealthSample>,
    pub endpoints: Vec<RouteEndpoint>,
    pub connected_lan_peers: Vec<PeerId>,
    /// Card 625abe6d slice 1: outbound dial attempts to stored peer
    /// endpoints that failed this refresh. Empty when every stored
    /// endpoint either connected or was already connected. A dial WAS
    /// attempted for each entry (≠ [`Self::peer_dial_skips`]).
    pub peer_dial_failures: Vec<PeerDialFailure>,
    /// Card 7e3c9a1f: endpoints SKIPPED this refresh because they are in
    /// dial-failure backoff — no dial was attempted. Surfaced (not
    /// silent) so the operator sees the backoff, but kept separate from
    /// `peer_dial_failures` so it is never miscounted/mislabelled as a
    /// failed dial.
    pub peer_dial_skips: Vec<PeerDialSkip>,
    /// #10: count of enrolled peers SKIPPED entirely this refresh because we
    /// haven't had fresh contact with them (no beacon within the freshness
    /// TTL) — dead Docker containers and abandoned self-scopes that linger in
    /// the trust store with stale endpoints. We don't dial them (each would
    /// burn a `PEER_DIAL_TIMEOUT` and bury live peers in failure noise), and a
    /// single count is surfaced rather than one skip per ghost (anti-spam).
    pub ghost_peers_skipped: usize,
}

impl RouteDiscoverySnapshot {
    /// #1247 slice 4b — the relay self-election DECISION (pure, so the
    /// daemon's trigger is unit-tested independent of the loop).
    ///
    /// A node should promote itself to a relay when it knows peers exist
    /// but can reach NONE of them right now: no live relay route AND no
    /// directly-connected LAN peer. That's an isolated node that could be
    /// a meeting point for other isolated peers.
    ///
    /// The rule stays deliberately simple because slice 4a made
    /// reachability EMPIRICAL — a wrong "yes" is harmless (an unreachable
    /// self-elected relay accumulates no connections and its gist entry
    /// goes stale), so there's no need to predict our own reachability.
    /// `become_relay` is idempotent, so re-evaluating "yes" every refresh
    /// just re-advertises.
    pub fn should_self_elect_as_relay(&self, enrolled_peers: usize) -> bool {
        let has_live_relay = self.health.iter().any(|sample| {
            sample.kind == TransportKind::Relay && sample.state == TransportHealthState::Healthy
        });
        enrolled_peers > 0 && self.connected_lan_peers.is_empty() && !has_live_relay
    }
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
        // Card 7e3c9a1f: refresh this handle's in-memory verifier registry
        // from the trust store BEFORE dialing. The daemon's route-refresh
        // handle is opened ONCE at startup; peers that converge later (via
        // the account-registry gist import) land in the trust STORE but
        // not in this long-lived handle's `PeerKeyRegistry`. Dialing such a
        // peer then fails the TLS handshake — "server cert pubkey is not
        // enrolled" — so no connection forms and the RoutedForwarder has no
        // link to deliver room broadcasts over. (`lan-send` sidesteps this
        // by opening a fresh handle per call, which is why direct sends
        // worked while room broadcast did not.) This is the same refresh
        // the non-daemon send path already does in `send_frame_to_room`;
        // the registry is a shared `Arc`, so the live LAN adapter's
        // verifier sees the newly-enrolled pubkeys.
        self.sync_account_peer_registry().await?;

        let (peer_dial_failures, peer_dial_skips, ghost_peers_skipped) =
            self.dial_stored_peer_endpoints().await?;

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
            peer_dial_skips,
            ghost_peers_skipped,
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
    async fn dial_stored_peer_endpoints(
        &self,
    ) -> Result<(Vec<PeerDialFailure>, Vec<PeerDialSkip>, usize), AircError> {
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
        // Card 7e3c9a1f: endpoints skipped because they are in dial-failure
        // backoff — surfaced on the snapshot SEPARATELY from `failures` so a
        // skip is never mislabelled/counted as an attempted-and-failed dial.
        let mut skips = Vec::new();
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
        let mut ghost_peers_skipped = 0usize;
        for peer in stored {
            if peer.peer_id == self.inner.identity.peer_id || connected.contains(&peer.peer_id) {
                continue;
            }
            // #10: skip GHOSTS — enrolled peers we haven't had fresh contact
            // with (no beacon within the freshness TTL). A live peer's registry
            // beacon refreshes `last_seen_ms` every cycle, so this never skips a
            // reachable peer; but dead containers / abandoned self-scopes keep a
            // stale endpoint forever, and dialing each one burns a
            // PEER_DIAL_TIMEOUT and drowns the live peers in failure noise. A
            // connected peer is already skipped above; a freshly-enrolled peer
            // has `last_seen_ms` floored to `added_at_ms`, so fresh adds dial
            // normally. Same TTL as the registry's liveness definition (one
            // source of truth).
            if is_ghost_peer(now_ms, peer.last_seen_ms) {
                ghost_peers_skipped += 1;
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

        // #9: prepend each peer's LEARNED real IP (harvested from an
        // authenticated inbound — see `AircInner::learned_ips`) as a PREFERRED
        // candidate, paired with the peer's STABLE advertised port (#8). A peer
        // that connected to us proved it's reachable at that IP, so trying
        // (learned_ip, stable_port) FIRST recovers a peer whose published
        // endpoint went stale (moved networks / pre-#8 record) before paying
        // dial timeouts on the dead rungs. Skipped when the candidate already
        // matches a stored endpoint (no new info). No security change: the IP
        // came from an enrolled peer's authenticated handshake, and the dial
        // still pins to the expected peer_id.
        if let Ok(learned) = self.inner.learned_ips.lock() {
            for (peer_id, eps) in merged.iter_mut() {
                let Some(learned_ip) = learned.get(peer_id).copied() else {
                    continue;
                };
                // Preferred-first: the learned IP is known-reachable, so try it
                // before the (possibly stale) published rungs. `.rev()` keeps the
                // candidates' relative (port) order once all are front-inserted.
                for candidate in learned_lan_candidates(eps, learned_ip).into_iter().rev() {
                    eps.insert(0, candidate);
                }
            }
        }

        // Per-peer dial work in first-seen (wire-root) order. Each
        // peer's endpoints are walked in stored (cost) order, breaking
        // on the first connect — that INNER walk stays serial (the
        // cost-order + stop-on-success contract the stored_endpoint_dial
        // tests pin). What is now CONCURRENT is ACROSS peers: dialing
        // peer B must not wait on peer A's 3s timeout (see
        // DIAL_CONCURRENCY — the serial-sum hang this fixes).
        let dial_list: Vec<(PeerId, Vec<RouteEndpoint>)> = order
            .into_iter()
            .filter_map(|peer_id| merged.remove(&peer_id).map(|eps| (peer_id, eps)))
            .collect();

        // Dial peers concurrently (bounded), then merge results in the
        // original peer order so operator display stays stable. `&self`
        // is shared immutably across the dials; connect_lan clones an
        // Arc adapter and every mutation (quarantine map, transport
        // health) sits behind its own brief lock, so concurrent dials to
        // distinct peers do not race.
        let mut dialed: Vec<(usize, PeerDialOutcome)> =
            futures::stream::iter(dial_list.into_iter().enumerate())
                .map(|(idx, (peer_id, endpoints))| async move {
                    (idx, self.dial_one_peer(peer_id, endpoints, now_ms).await)
                })
                .buffer_unordered(DIAL_CONCURRENCY)
                .collect()
                .await;
        dialed.sort_by_key(|(idx, _)| *idx);
        for (_, (peer_failures, peer_skips)) in dialed {
            failures.extend(peer_failures);
            skips.extend(peer_skips);
        }
        Ok((failures, skips, ghost_peers_skipped))
    }

    /// Dial ONE peer's stored endpoints in cost order, breaking on the
    /// first connect. Returns this peer's dial failures + quarantine
    /// skips. Extracted from `dial_stored_peer_endpoints` so the
    /// per-peer dials run concurrently; the inner endpoint walk here
    /// stays serial because cost-order + stop-on-first-success is the
    /// pinned contract.
    async fn dial_one_peer(
        &self,
        peer_id: PeerId,
        endpoints: Vec<RouteEndpoint>,
        now_ms: u64,
    ) -> PeerDialOutcome {
        let mut failures = Vec::new();
        let mut skips = Vec::new();
        for endpoint in &endpoints {
            match endpoint {
                RouteEndpoint::LanTcp { addr } | RouteEndpoint::TailscaleTcp { addr } => {
                    let addr = *addr;
                    // Card 7e3c9a1f: skip endpoints still inside their dial-
                    // failure backoff window. A daemon that restarted on a new
                    // port leaves its old `addr` in every peer's trust store
                    // until the registry re-converges; without this skip each
                    // refresh re-pays PEER_DIAL_TIMEOUT on that corpse, starving
                    // the dial to the live endpoint. The freshest (most likely
                    // live) endpoint is listed first and is never quarantined
                    // unless it itself just failed, so this never blocks a
                    // genuinely reachable peer.
                    //
                    // The skip is SURFACED, not silent — but on the SEPARATE
                    // `peer_dial_skips` channel, NOT as a `PeerDialFailure`. No
                    // dial was attempted, so reporting it as a failure would emit
                    // false `PeerDialFailed` warnings every refresh and inflate
                    // `airc transport health`'s failure count. The operator still
                    // sees "in backoff" via the skips channel — honest, not a
                    // clean-list lie and not an over-report.
                    if let Some(remaining_ms) =
                        self.dial_quarantine_remaining_ms(peer_id, addr, now_ms)
                    {
                        skips.push(PeerDialSkip {
                            peer_id,
                            endpoint: endpoint.clone(),
                            remaining_ms,
                        });
                        continue;
                    }
                    // #1120 sentinel blocking-2: connect_lan has no inner
                    // timeout, and a SYN-dropping firewall (the default posture
                    // of the NATs this card exists to cross) hangs the OS connect
                    // for ~21-130s per endpoint. Bound every dial; a timeout is a
                    // recorded failure like any other.
                    match tokio::time::timeout(PEER_DIAL_TIMEOUT, self.connect_lan(addr, peer_id))
                        .await
                    {
                        Ok(Ok(())) => {
                            // Card 7e3c9a1f: a live connect lifts any prior
                            // quarantine so a flapped-but-recovered endpoint is
                            // immediately eligible again next refresh.
                            self.dial_quarantine_record_success(peer_id, addr);
                            break;
                        }
                        Ok(Err(error)) => {
                            self.dial_quarantine_record_failure(peer_id, addr, now_ms);
                            failures.push(PeerDialFailure {
                                peer_id,
                                endpoint: endpoint.clone(),
                                error: error.to_string(),
                            });
                        }
                        Err(_elapsed) => {
                            self.dial_quarantine_record_failure(peer_id, addr, now_ms);
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
                // #1247 slice 2: a peer advertising a relay means "reach me
                // (and others) through it." Connect to the relay as a
                // cross-boundary route — direct LAN/tailscale endpoints are
                // listed first (cost order), so we reach here only after they
                // failed. `connect_relay` is idempotent, so a relay advertised
                // by several peers connects once; its identity is mTLS-pinned
                // (enrolled by the `sync_account_peer_registry` at the top of
                // refresh for a self-advertised relay). Same dial-failure
                // backoff as direct endpoints, keyed on the RELAY's identity so
                // a dead relay isn't re-dialed every refresh. A legacy
                // peer-id-less URL is not connectable — skipped here (surfaced
                // upstream as not-pinnable), never a silent dial to an
                // unauthenticated relay.
                RouteEndpoint::Relay { .. } => {
                    let Some((relay_peer, relay_addr)) = endpoint.connectable_relay() else {
                        continue;
                    };
                    if let Some(remaining_ms) =
                        self.dial_quarantine_remaining_ms(relay_peer, relay_addr, now_ms)
                    {
                        skips.push(PeerDialSkip {
                            peer_id,
                            endpoint: endpoint.clone(),
                            remaining_ms,
                        });
                        continue;
                    }
                    match tokio::time::timeout(
                        PEER_DIAL_TIMEOUT,
                        self.connect_relay(relay_addr, relay_peer),
                    )
                    .await
                    {
                        Ok(Ok(())) => {
                            self.dial_quarantine_record_success(relay_peer, relay_addr);
                            // A relay route to this peer is established; stop
                            // walking this peer's remaining (lower-priority)
                            // endpoints.
                            break;
                        }
                        Ok(Err(error)) => {
                            self.dial_quarantine_record_failure(relay_peer, relay_addr, now_ms);
                            failures.push(PeerDialFailure {
                                peer_id,
                                endpoint: endpoint.clone(),
                                error: format!("relay connect: {error}"),
                            });
                        }
                        Err(_elapsed) => {
                            self.dial_quarantine_record_failure(relay_peer, relay_addr, now_ms);
                            failures.push(PeerDialFailure {
                                peer_id,
                                endpoint: endpoint.clone(),
                                error: format!(
                                    "relay dial timed out after {}s (relay unreachable or \
                                     firewall drops SYN)",
                                    PEER_DIAL_TIMEOUT.as_secs()
                                ),
                            });
                        }
                    }
                }
                // UDP/WebRTC/Reticulum dialing lands in later slices.
                // Variants enumerated, not wildcarded, per the production
                // no-silent-fallback clippy gate: a FUTURE endpoint variant
                // must force this match to be revisited, not silently fall
                // into "skip" (the wildcard slipped past #1120's review when
                // the merge-push CI run was hand-cancelled — caught by #1121).
                RouteEndpoint::Udp { .. }
                | RouteEndpoint::Reticulum { .. }
                | RouteEndpoint::WebRtcSignaling { .. } => continue,
            }
        }
        (failures, skips)
    }

    /// Snapshot the last known discovery state without mutating
    /// route health.
    pub async fn route_discovery_snapshot(&self) -> Result<RouteDiscoverySnapshot, AircError> {
        Ok(RouteDiscoverySnapshot {
            health: self.transport_health()?,
            endpoints: self.route_endpoints()?,
            connected_lan_peers: self.connected_lan_peers().await,
            peer_dial_failures: Vec::new(),
            peer_dial_skips: Vec::new(),
            ghost_peers_skipped: 0,
        })
    }

    /// Card 7e3c9a1f: backoff remaining for `(peer_id, addr)`, or `None`
    /// when not quarantined. Keyed per-peer so a recycled container IP
    /// under a new peer is not shadow-banned by a dead peer's failure.
    /// Brief lock, never held across an await.
    fn dial_quarantine_remaining_ms(
        &self,
        peer_id: PeerId,
        addr: std::net::SocketAddr,
        now_ms: u64,
    ) -> Option<u64> {
        let guard = self
            .inner
            .dial_quarantine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remaining_ms(&(peer_id, addr), now_ms)
    }

    /// Card 7e3c9a1f: stamp a failed dial to `(peer_id, addr)` (starts /
    /// doubles the backoff).
    fn dial_quarantine_record_failure(
        &self,
        peer_id: PeerId,
        addr: std::net::SocketAddr,
        now_ms: u64,
    ) {
        let mut guard = self
            .inner
            .dial_quarantine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.record_failure((peer_id, addr), now_ms);
    }

    /// Card 7e3c9a1f: clear any quarantine on `(peer_id, addr)` after a
    /// live connect.
    fn dial_quarantine_record_success(&self, peer_id: PeerId, addr: std::net::SocketAddr) {
        let mut guard = self
            .inner
            .dial_quarantine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.record_success(&(peer_id, addr));
    }

    async fn connected_lan_peers(&self) -> Vec<PeerId> {
        let adapter = self.inner.lan_tcp.lock().await.clone();
        match adapter {
            Some(adapter) => adapter.connected_peers().await,
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches (#9): the learn-live-address candidate builder. A peer
    // that connected to us teaches its real IP; we must produce
    // (learned_ip, advertised_port) as a NEW candidate so a peer whose
    // published IP went stale is still dialable — but add nothing when the
    // learned IP already matches a stored endpoint (no churn).
    #[test]
    fn learned_candidates_pair_learned_ip_with_advertised_ports() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let stale = RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(10, 0, 1, 16), 50000)),
        };
        let learned = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 232));

        // Stale published IP + a learned IP → one candidate on the same port.
        let cands = learned_lan_candidates(std::slice::from_ref(&stale), learned);
        assert_eq!(
            cands,
            vec![RouteEndpoint::LanTcp {
                addr: SocketAddr::from((Ipv4Addr::new(192, 168, 1, 232), 50000)),
            }],
            "learned IP must pair with the peer's advertised (stable) port"
        );

        // Learned IP already matches the stored endpoint → nothing to add.
        let already = RouteEndpoint::LanTcp {
            addr: SocketAddr::from((Ipv4Addr::new(192, 168, 1, 232), 50000)),
        };
        assert!(
            learned_lan_candidates(&[already], learned).is_empty(),
            "no new candidate when the learned IP already matches a stored endpoint"
        );
    }

    // what this catches (#10): the ghost boundary. A peer with fresh contact
    // (beacon within the TTL) must remain dialable; one past the TTL must be
    // classified a ghost and skipped — so the dialer stops burning timeouts on
    // dead containers/scopes. A flipped comparison or a future-skewed last_seen
    // underflow would regress here.
    #[test]
    fn ghost_classification_respects_freshness_ttl() {
        let ttl = crate::account_registry::DEFAULT_PEER_FRESHNESS_TTL_MS;
        let now = 10 * ttl; // comfortably past any boundary arithmetic
        assert!(!is_ghost_peer(now, now), "just-seen peer is live");
        assert!(
            !is_ghost_peer(now, now - ttl),
            "seen exactly at the TTL edge is still live (boundary is strictly greater)"
        );
        assert!(
            is_ghost_peer(now, now - ttl - 1),
            "seen past the TTL is a ghost"
        );
        assert!(
            !is_ghost_peer(now, now + 5_000),
            "a future last_seen (clamped on import; skew) must read fresh, never underflow"
        );
    }

    fn snapshot(
        health: Vec<TransportHealthSample>,
        connected_lan_peers: Vec<PeerId>,
    ) -> RouteDiscoverySnapshot {
        RouteDiscoverySnapshot {
            health,
            endpoints: Vec::new(),
            connected_lan_peers,
            peer_dial_failures: Vec::new(),
            peer_dial_skips: Vec::new(),
            ghost_peers_skipped: 0,
        }
    }

    /// what this catches (#1247 slice 4b): a node that knows peers exist
    /// but can reach NONE of them (no relay, no LAN peer) self-elects as a
    /// relay — the isolated-node-becomes-a-meeting-point case.
    #[test]
    fn isolated_node_with_enrolled_peers_self_elects() {
        assert!(snapshot(Vec::new(), Vec::new()).should_self_elect_as_relay(2));
    }

    /// A directly-connected LAN peer means the node can already reach the
    /// mesh — no need to become a relay.
    #[test]
    fn a_connected_lan_peer_means_no_election() {
        assert!(!snapshot(Vec::new(), vec![PeerId::new()]).should_self_elect_as_relay(2));
    }

    /// A live relay route means the node already has a meeting point —
    /// don't elect a competing one.
    #[test]
    fn a_live_relay_means_no_election() {
        let health = vec![TransportHealthSample::healthy_direct(TransportKind::Relay)];
        assert!(!snapshot(health, Vec::new()).should_self_elect_as_relay(2));
    }

    /// Grid-of-one: no enrolled peers = no mesh to join = nothing to
    /// relay for. Never elect (else every fresh, peerless node spins up a
    /// pointless relay).
    #[test]
    fn grid_of_one_never_elects() {
        assert!(!snapshot(Vec::new(), Vec::new()).should_self_elect_as_relay(0));
    }
}
