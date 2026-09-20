//! LAN presence beacon — gh-free same-network discovery.
//!
//! ## Why this exists
//!
//! Before this module, airc had ZERO LAN-local discovery — no mDNS, no
//! multicast, no broadcast. Two peers on the same /24 could only find each
//! other through the GitHub rendezvous (budget-capped, beacons stale in
//! minutes) or through cached endpoints (invalidated by churn). Measured
//! live 2026-09-04: a peer's Mac was on the same LAN, its daemon LISTENING,
//! while this node reported `0 routes, 96 enrolled peers` — a human closed
//! the gap with `arp` + a hand-computed port + `airc dial`, which no user
//! can be asked to do.
//!
//! That gap is also what made the gh channel a single point of failure: LAN
//! convergence rode the same request budget as SOS and registry sync, so a
//! busy hour could orphan peers standing three meters apart. Per the
//! operator directive behind this module: the mesh must ride nodes coming
//! on/offline continuously, and must never spend gh budget on something the
//! local wire can say for free.
//!
//! ## What it is
//!
//! A ~100-byte JSON datagram on an administratively-scoped multicast group
//! (RFC 2365), announced on a jittered fixed cadence and harvested by every
//! listening scope:
//!
//! ```json
//! {"v":1,"peer_id":"…","port":61539,"stamp_ms":…}
//! ```
//!
//! ## What it is NOT (security posture)
//!
//! A beacon is a HINT, never trust. Receivers act on it only by attempting
//! the existing AUTHENTICATED dial, pinned to an ENROLLED peer identity —
//! the same posture as learn-live-address (#9). The consumer ignores
//! beacons from peers it has not enrolled: discovery is not authorization.
//! A forged beacon can cause at most one pinned dial that fails its
//! handshake, bounded further by the existing dial quarantine. The receiver
//! also takes the peer's address from the DATAGRAM SOURCE, never from any
//! claimed field — the port is claimed (a socket cannot claim a port it
//! doesn't hold without failing the subsequent dial), the IP is observed.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use uuid::Uuid;

/// Administratively-scoped multicast group (RFC 2365 239/8 — never routed
/// beyond the local network). Deliberately NOT 224.0.0.251:5353 (mDNS) or
/// 239.255.255.250:1900 (SSDP): sharing a group with chatty household
/// protocols means parsing their traffic forever and fighting their OS
/// daemons for the port.
pub const LAN_PRESENCE_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 77, 77);

/// One port for the whole mesh. In the dynamic range's floor region rather
/// than the identity-derived space (49152 + hash), so it can never collide
/// with any peer's `stable_lan_port`.
pub const LAN_PRESENCE_PORT: u16 = 47700;

/// Announce cadence. 15s bounds same-LAN convergence for a freshly-online
/// node to one beacon interval + one route-refresh pick-up — seconds, not
/// the gh rendezvous's minutes. At ~100 bytes/beacon this is ~7 B/s of
/// multicast: three orders of magnitude below anything a LAN notices.
/// Deliberately far below the 10-minute peer-freshness TTL so presence can
/// never flap stale between announcements (the cadence-vs-TTL inversion
/// class: a TTL shorter than the publish interval marks healthy peers dead).
pub const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(15);

/// Fraction of the interval used as per-node jitter, 802.11-style desync:
/// after a shared wake event (power restored, AP reboot) every node would
/// otherwise beacon on the same tick forever. The offset is derived from
/// the announcing peer's id — stable per node, uniform across nodes, no
/// RNG dependency.
pub const JITTER_FRACTION: f64 = 0.2;

/// Wire payload. `v` is a compatibility floor, not an exact match — a
/// receiver admits any `v >= 1` and ignores fields it doesn't know, so old
/// nodes keep hearing new nodes (the version-skew posture: tolerate what
/// you don't understand, loudly at debug, never a parse abort).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanPresenceBeacon {
    pub v: u32,
    pub peer_id: Uuid,
    /// The announcer's live LISTEN port — claimed, and safe to trust as a
    /// dial target because a wrong claim just fails the pinned handshake.
    pub port: u16,
    pub stamp_ms: u64,
}

/// A beacon admitted by [`admit_beacon`]: who, observed-from where, and the
/// claimed listen port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanPresenceHint {
    pub peer_id: Uuid,
    /// The datagram's SOURCE address — observed, not claimed.
    pub ip: IpAddr,
    pub port: u16,
}

/// Pure admission filter, unit-tested without sockets: parse, version
/// floor, self-filter, source-address substitution. Enrollment filtering is
/// deliberately NOT here — the trust registry lives above this crate, so
/// the consumer applies it (and this function's contract says every hint
/// still requires it).
pub fn admit_beacon(datagram: &[u8], src: SocketAddr, self_peer: Uuid) -> Option<LanPresenceHint> {
    let beacon: LanPresenceBeacon = serde_json::from_slice(datagram).ok()?;
    if beacon.v < 1 {
        return None;
    }
    // Every scope on this machine shares the group and hears its own
    // announcements; self-echo is normal, not signal.
    if beacon.peer_id == self_peer {
        return None;
    }
    Some(LanPresenceHint {
        peer_id: beacon.peer_id,
        ip: src.ip(),
        port: beacon.port,
    })
}

/// Per-node announce offset within [`ANNOUNCE_INTERVAL`] — the jitter.
/// Derived from the peer id so it is stable for a node's lifetime and
/// uniformly spread across nodes, with no RNG in the dependency tree.
pub fn announce_jitter(self_peer: Uuid) -> Duration {
    let max_jitter_ms = (ANNOUNCE_INTERVAL.as_millis() as f64 * JITTER_FRACTION) as u64;
    if max_jitter_ms == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(self_peer.as_u128() as u64 % max_jitter_ms)
}

/// Build the shared receive socket: SO_REUSEADDR so EVERY scope on one
/// machine (project + machine-account) can hear the group — without it the
/// first daemon to bind starves the rest, which is the multi-scope split
/// all over again at the discovery layer. socket2 (already in the tree) is
/// required because std/tokio cannot set reuse before bind.
fn bind_presence_socket() -> std::io::Result<UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, LAN_PRESENCE_PORT)).into())?;
    socket.join_multicast_v4(&LAN_PRESENCE_GROUP, &Ipv4Addr::UNSPECIFIED)?;
    UdpSocket::from_std(socket.into())
}

/// Spawn the announcer + receiver pair. Returns after binding (loud on
/// failure — a node that cannot join the group must SAY so, not silently
/// become undiscoverable); the tasks run until the runtime drops them.
///
/// `on_hint` fires for every admitted beacon. The consumer owns enrollment
/// filtering and where the hint lands (airc-lib feeds `learned_ips`, the
/// same map learn-live-address feeds, so the existing dial rungs pick it up
/// with no new plumbing).
pub fn spawn_lan_presence(
    self_peer: Uuid,
    advertised_port: u16,
    on_hint: impl Fn(LanPresenceHint) + Send + Sync + 'static,
) -> std::io::Result<()> {
    let recv_socket = bind_presence_socket()?;

    // Receiver: harvest hints forever. Datagrams that fail admission are
    // dropped silently BY DESIGN — a multicast group is a public hallway,
    // and logging every stranger's packet is noise, not honesty. Admitted
    // hints are the consumer's to log (edge-triggered, on change).
    tokio::spawn(async move {
        let mut buf = [0u8; 512];
        loop {
            match recv_socket.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    if let Some(hint) = admit_beacon(&buf[..len], src, self_peer) {
                        on_hint(hint);
                    }
                }
                Err(_transient) => {
                    // Transient recv errors (ICMP-triggered on Windows) must
                    // not kill the receiver; a dead receiver is a silently
                    // undiscoverable node — the exact failure this module
                    // exists to remove. This crate is log-free by convention
                    // (errors surface via Result); a spawned loop has no
                    // Result to return, so it pauses and continues, and the
                    // CONSUMER's edge-triggered hint logging is the visible
                    // signal that discovery is alive.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });

    // Announcer: its own socket (ephemeral source port), so the shared
    // receive socket never doubles as a send path.
    tokio::spawn(async move {
        let target = SocketAddr::from((LAN_PRESENCE_GROUP, LAN_PRESENCE_PORT));
        tokio::time::sleep(announce_jitter(self_peer)).await;
        let mut tick = tokio::time::interval(ANNOUNCE_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let beacon = LanPresenceBeacon {
                v: 1,
                peer_id: self_peer,
                port: advertised_port,
                stamp_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            };
            let Ok(payload) = serde_json::to_vec(&beacon) else {
                continue;
            };
            match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await {
                Ok(sender) => {
                    // Send failures (interface down, no route) are transient
                    // by nature; the next tick retries. No log by crate
                    // convention — and on a machine with no network a
                    // warn-per-tick would cry wolf every 15s forever.
                    let _best_effort = sender.send_to(&payload, target).await;
                }
                Err(_transient) => {
                    // Next tick retries with a fresh socket.
                }
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the wire contract. A beacon must round-trip, and a
    // receiver must take the IP from the DATAGRAM SOURCE (observed) while
    // taking the port from the payload (claimed-but-verified-by-dial) — the
    // asymmetry is the security posture, so it gets pinned.
    #[test]
    fn admitted_hint_uses_source_ip_and_claimed_port() {
        let announcer = Uuid::new_v4();
        let receiver = Uuid::new_v4();
        let beacon = LanPresenceBeacon {
            v: 1,
            peer_id: announcer,
            port: 61539,
            stamp_ms: 1,
        };
        let bytes = serde_json::to_vec(&beacon).unwrap();
        let src: SocketAddr = "192.168.1.232:52000".parse().unwrap();

        let hint = admit_beacon(&bytes, src, receiver).expect("valid beacon admitted");
        assert_eq!(hint.peer_id, announcer);
        assert_eq!(
            hint.ip,
            "192.168.1.232".parse::<IpAddr>().unwrap(),
            "IP must be the observed datagram source, never a claimed field"
        );
        assert_eq!(hint.port, 61539, "port is the claimed listen port");
    }

    // what this catches: every scope on a machine shares the multicast group
    // and hears its own announcements. Without the self-filter each node
    // would 'learn' its own IP as a peer hint every 15s, forever.
    #[test]
    fn own_echo_is_not_a_hint() {
        let me = Uuid::new_v4();
        let beacon = LanPresenceBeacon {
            v: 1,
            peer_id: me,
            port: 1,
            stamp_ms: 1,
        };
        let bytes = serde_json::to_vec(&beacon).unwrap();
        let src: SocketAddr = "192.168.1.5:52000".parse().unwrap();
        assert!(admit_beacon(&bytes, src, me).is_none());
    }

    // what this catches: version-skew tolerance. A v2 beacon with fields
    // this build has never heard of must still be admitted (v-floor, extra
    // fields ignored) — old nodes must keep hearing new nodes, or a mesh
    // upgrade partitions discovery by build age.
    #[test]
    fn future_versions_and_unknown_fields_are_admitted() {
        let announcer = Uuid::new_v4();
        let raw = format!(
            r#"{{"v":2,"peer_id":"{announcer}","port":5,"stamp_ms":9,"new_field":"ignored"}}"#
        );
        let src: SocketAddr = "10.0.0.9:1000".parse().unwrap();
        let hint = admit_beacon(raw.as_bytes(), src, Uuid::new_v4()).expect("v2 admitted");
        assert_eq!(hint.peer_id, announcer);

        // …but garbage and v0 are refused.
        assert!(admit_beacon(b"not json", src, Uuid::new_v4()).is_none());
        let v0 = format!(r#"{{"v":0,"peer_id":"{announcer}","port":5,"stamp_ms":9}}"#);
        assert!(admit_beacon(v0.as_bytes(), src, Uuid::new_v4()).is_none());
    }

    // what this catches: the desync property. Jitter must be stable per
    // node, bounded by JITTER_FRACTION of the interval, and actually spread
    // distinct nodes (probabilistically — pinned with fixed ids).
    #[test]
    fn jitter_is_stable_bounded_and_spreads_nodes() {
        let a = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
        let b = Uuid::from_u128(0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0001);
        let max =
            Duration::from_millis((ANNOUNCE_INTERVAL.as_millis() as f64 * JITTER_FRACTION) as u64);
        assert_eq!(announce_jitter(a), announce_jitter(a), "stable per node");
        assert!(
            announce_jitter(a) < max && announce_jitter(b) < max,
            "bounded"
        );
        assert_ne!(
            announce_jitter(a),
            announce_jitter(b),
            "distinct nodes spread"
        );
    }
}
