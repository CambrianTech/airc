use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use airc_lib::{
    coordinator_snapshot_store, machine_account_home, resolve_mesh_identity, Airc,
    CoordinatorConfig, PresenceBeacon, DEFAULT_PEER_FRESHNESS_TTL_MS,
};
use airc_store::SqliteEventStore;

pub fn run_lan_ip() -> Result<(), Box<dyn Error>> {
    if let Some(ip) = detect_lan_ip() {
        println!("{ip}");
    }
    Ok(())
}

/// Best-effort primary LAN IPv4 of this host (the source address the OS
/// would use to reach the internet). `pub(crate)` so the daemon can
/// advertise it as its dialable LAN endpoint in the account-registry
/// beacon — the endpoint-in-beacon half of same-account auto-discovery.
pub(crate) fn detect_lan_ip() -> Option<Ipv4Addr> {
    // BIGMAMA review fix on #1201: the daemon previously chose
    // detect_tailscale_ip().or_else(detect_lan_ip), so Docker-bridge
    // IPs in 172.18.0.x were never advertised because containers had
    // Tailscale up. This PR publishes BOTH (LAN + Tailscale), which
    // ALSO publishes the Docker bridge IP from inside containers —
    // re-flooding peer trust stores with the same 172.18.0.x ghosts
    // we cleaned up via `airc peer prune`.
    //
    // Targeted fix: if we're running INSIDE a container, the OS's
    // "primary LAN" is the Docker bridge — not a real LAN — so we
    // SHOULD NOT publish it as a dialable endpoint. Real hosts with
    // 172.16/12 corporate LANs continue to work; only containers
    // change behavior.
    if in_container() {
        return None;
    }
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if is_routable_lan_ipv4(ip) => Some(ip),
        IpAddr::V4(_) | IpAddr::V6(_) => None,
    }
}

fn is_routable_lan_ipv4(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified()
}

/// The IPv4 the daemon should ADVERTISE as its dialable LAN endpoint.
///
/// Card 7e3c9a1f — the container-reachability seam. `detect_lan_ip()`
/// deliberately bails inside a container (its only "LAN" is the Docker
/// bridge, unreachable from any other machine — the `172.18.x` ghost
/// flood). But a containerized node still needs to advertise SOMETHING
/// routable: on the same Docker network its bridge IP IS reachable by
/// sibling containers, and for the real grid the HOST (where detection
/// works) hands in the host's LAN/Tailscale endpoint. `AIRC_ADVERTISE_IP`
/// is that explicit handoff — the launcher or the node entrypoint sets
/// it; the daemon advertises it verbatim. Absent the override we fall
/// back to host auto-detection, so non-container hosts are unchanged.
///
/// This is the "advertise the host endpoints you were given" half of the
/// container fix — the route/dial layer is untouched; only the source of
/// the advertised address changes.
pub(crate) fn advertise_lan_ip() -> Option<Ipv4Addr> {
    if let Some(ip) = env_advertise_ip() {
        return Some(ip);
    }
    detect_lan_ip()
}

/// Parse `AIRC_ADVERTISE_IP` into an IPv4, or `None` when unset/blank.
/// A SET-but-unparseable value is loud (no silent fallback): we warn and
/// return `None` so the caller falls through to auto-detection rather
/// than silently advertising garbage.
fn env_advertise_ip() -> Option<Ipv4Addr> {
    match std::env::var("AIRC_ADVERTISE_IP") {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<Ipv4Addr>() {
            Ok(ip) => Some(ip),
            Err(_) => {
                eprintln!(
                    "airc daemon: AIRC_ADVERTISE_IP='{raw}' is not a valid IPv4 address — \
                     ignoring it and falling back to host auto-detection"
                );
                None
            }
        },
        _ => None,
    }
}

/// True when this process is running inside an OCI container (Docker,
/// Podman, containerd). Two cheap probes:
///   - `/.dockerenv` exists (Docker writes it on container start)
///   - PID 1's cgroup membership mentions docker/containerd/kubepods
///
/// We're intentionally conservative — false negatives just mean a
/// container-bound process MIGHT publish its bridge IP, which is the
/// pre-fix behavior we're improving. False positives (a real host that
/// happens to have /.dockerenv) means a real LAN IP won't be advertised
/// and the daemon falls back to Tailscale, which is the safe default
/// for the `100.x`-everywhere airc mesh.
fn in_container() -> bool {
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }
    if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
        let cg = cgroup.to_ascii_lowercase();
        if cg.contains("docker") || cg.contains("containerd") || cg.contains("kubepods") {
            return true;
        }
    }
    false
}

/// Best-effort Tailscale IPv4 (the host's `100.64.0.0/10` CGNAT address),
/// if a Tailscale interface is up. Same source-address trick as
/// `detect_lan_ip`, but aimed at Tailscale's MagicDNS resolver
/// (`100.100.100.100`) — an address only routable over the Tailscale
/// interface, so the OS selects that interface and `local_addr()` reveals
/// our `100.x`. Returns `None` when Tailscale is down (the OS falls back to
/// the LAN interface, whose IP fails the CGNAT check).
///
/// The daemon prefers this over the LAN IP for its advertised endpoint:
/// every node on the same gh-account mesh is on Tailscale, so a `100.x`
/// endpoint is dialable from ANY network and traverses NAT/firewalls, while
/// a `192.168.x` endpoint only works same-subnet and dies behind a firewall.
pub(crate) fn detect_tailscale_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket
        .connect((Ipv4Addr::new(100, 100, 100, 100), 80))
        .ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if is_tailscale_ipv4(ip) => Some(ip),
        IpAddr::V4(_) | IpAddr::V6(_) => None,
    }
}

/// Tailscale's CGNAT range is `100.64.0.0/10` (100.64.0.0 – 100.127.255.255).
fn is_tailscale_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

// ===========================================================================
// `airc network` — the one-glance mesh-liveness + reachability view.
//
// Before this command, answering "why can't I reach peer X?" meant
// hand-decoding raw account-registry gists: which machine is live, on
// which channels, with what dialable endpoint, and — the part that
// actually bites — whether THIS scope's default room has any other peer
// in it at all. Two machines on the same account silently talk past each
// other when their default-room pointers differ (one on #cambriantech,
// the others on #general): every send lands in a room empty from the
// peers' side, with zero signal.
//
// `network` collapses that into one read-only view over state the daemon
// already holds: the coordinator beacon store (presence + channels +
// freshness), the peer trust store (dialable endpoints), and this
// scope's own default room. No `gh` call, no publish, no mutation — it
// reports what this daemon knows now. (`airc registry sync` is the verb
// that refreshes that knowledge from the rendezvous.)
// ===========================================================================

/// The reachability verdict for this scope's default room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConvergenceStatus {
    /// True if at least one OTHER live peer subscribes to the default room.
    pub reachable: bool,
    /// Number of other live peers (self already excluded by the caller).
    pub other_peer_count: usize,
    /// Sorted, de-duplicated rooms where the other live peers ARE — the
    /// "they're over there" hint shown when `reachable` is false.
    pub other_rooms: Vec<String>,
}

/// Decide convergence from this scope's default room and the
/// subscribed-channel sets of the OTHER live peers (the caller has
/// already excluded this scope's own beacon).
///
/// Pure so the "0 reachable peers" warning is a pinnable contract:
/// mutating `reachable` to a constant, or dropping the `== my_room`
/// match, breaks the unit tests below — not a downstream operator who
/// trusted an empty room.
pub(crate) fn convergence_status(
    my_room: &str,
    other_live_peer_channels: &[Vec<String>],
) -> ConvergenceStatus {
    let reachable = other_live_peer_channels
        .iter()
        .any(|channels| channels.iter().any(|channel| channel == my_room));
    let other_rooms: BTreeSet<String> =
        other_live_peer_channels.iter().flatten().cloned().collect();
    ConvergenceStatus {
        reachable,
        other_peer_count: other_live_peer_channels.len(),
        other_rooms: other_rooms.into_iter().collect(),
    }
}

/// Best-effort OS/role hint from a beacon's `scope_home` path shape.
/// Purely cosmetic — the load-bearing identity is `peer_id`.
fn machine_label(scope_home: &Path) -> &'static str {
    let path = scope_home.to_string_lossy();
    if path.starts_with("\\\\?\\") || path.contains(":\\") {
        "windows"
    } else if path.contains("/Users/") {
        "macos"
    } else if path.starts_with("/node/") {
        "container"
    } else {
        "linux"
    }
}

/// Human "Ns ago" / "Nm ago" / "Nh ago" / "Nd ago" from a millisecond
/// age. Saturating at the caller; bucketed at the natural rollovers.
fn humanize_age_ms(age_ms: u64) -> String {
    let secs = age_ms / 1_000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Channel names a beacon subscribes to, as plain strings for display
/// and convergence comparison (apples-to-apples with `Room::name`).
fn beacon_channels(beacon: &PresenceBeacon) -> Vec<String> {
    beacon
        .subscribed_channels
        .iter()
        .map(|channel| channel.as_str().to_string())
        .collect()
}

/// First 8 chars of a UUID-shaped id — enough to disambiguate at a
/// glance without the line-wrapping a full UUID forces.
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// One-line breakdown of stale beacons by machine label, e.g.
/// `38 container, 3 macos, 1 windows`. Sorted by descending count so
/// the dominant ghost class (usually dead containers) reads first, with
/// the label as a stable tie-break.
fn stale_summary(stale: &[PresenceBeacon]) -> String {
    let mut by_label: BTreeMap<&'static str, usize> = BTreeMap::new();
    for beacon in stale {
        *by_label
            .entry(machine_label(&beacon.scope_home))
            .or_insert(0) += 1;
    }
    let mut pairs: Vec<(&'static str, usize)> = by_label.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    pairs
        .iter()
        .map(|(label, count)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `airc network` — print the live/stale mesh view and the convergence
/// verdict for this scope's default room. `show_all_stale` lists every
/// stale beacon (the `--all` flag); otherwise stale is one summary line
/// so the live set + convergence verdict stay the headline.
pub async fn run_network(home: &Path, show_all_stale: bool) -> Result<(), Box<dyn Error>> {
    let airc = Airc::open(home).await?;
    let me = airc.peer_id();
    // READ-ONLY: `default_room` (not `current_room`) — `current_room`
    // lazily subscribes to #general + publishes a presence beacon on a
    // fresh scope, which would make this "no mutation" inspection
    // command mutate. `None` here = no default room subscribed yet.
    // (#1217 cross-review, M5.)
    let current = airc.peek_default_room().await?;

    // ONE machine-account store (§3.3) — the same events.sqlite the
    // daemon's coordinator writes beacons into. Reading it shows what
    // THIS daemon knows about the mesh, with no gh round-trip.
    let db_path = machine_account_home(home).join("events.sqlite");
    let store = Arc::new(SqliteEventStore::open_path(&db_path).await?);
    let identity = resolve_mesh_identity(store.as_ref())
        .await?
        .as_mesh_identity();

    let now = now_ms();
    // Liveness lens: the 10-min registry FRESHNESS window, NOT the 60s
    // coordinator heartbeat TTL. Cross-machine beacons publish on a
    // ~120s cadence, so a 60s window flaps a healthy remote peer
    // stale/live every cycle and misreports the grid. The registry's own
    // freshness TTL is the right "is this peer alive on the grid" lens —
    // the same window its stale-beacon prune uses.
    let config = CoordinatorConfig {
        heartbeat_ttl_ms: DEFAULT_PEER_FRESHNESS_TTL_MS,
        ..CoordinatorConfig::default()
    };
    let snapshot = coordinator_snapshot_store(store.as_ref(), &identity, &config, now).await?;

    // Endpoints live on the trust store, keyed by peer_id; index them so
    // each live peer line can show what route discovery would dial.
    let trusted = airc_trust::load(home).await.unwrap_or_default();
    let endpoints_for = |peer_id| -> String {
        trusted
            .iter()
            .find(|p| p.peer_id == peer_id)
            .and_then(|p| p.endpoints_json.as_deref())
            .map(|json| match airc_lib::endpoints_from_json(json) {
                Ok(endpoints) if !endpoints.is_empty() => format!("   dial={endpoints:?}"),
                Ok(_) => String::new(),
                Err(error) => format!("   dial=<undecodable: {error}>"),
            })
            .unwrap_or_default()
    };

    let room_display = match current.as_ref() {
        Some(room) => format!("#{}", room.name),
        None => "(none — run `airc join`)".to_string(),
    };
    println!(
        "mesh: {identity}    you: {me_short}    default room: {room_display}",
        identity = identity.as_str(),
        me_short = short(&me.to_string()),
    );
    println!();

    // LIVE peers. Self is marked so the operator never mistakes their
    // own beacon for a peer; only OTHER beacons feed the convergence
    // check.
    let mut other_live_channels: Vec<Vec<String>> = Vec::new();
    println!("LIVE peers ({}):", snapshot.live.len());
    if snapshot.live.is_empty() {
        println!("  (none — run `airc registry sync` to pull fresh beacons from the rendezvous)");
    }
    for beacon in &snapshot.live {
        let is_self = beacon.peer_id == me;
        let channels = beacon_channels(beacon);
        if !is_self {
            other_live_channels.push(channels.clone());
        }
        let age = humanize_age_ms(now.saturating_sub(beacon.heartbeat_at_ms));
        println!(
            "  {peer}  {machine:<9}  rooms=[{rooms}]  {age}{dial}{me_tag}",
            peer = short(&beacon.peer_id.to_string()),
            machine = machine_label(&beacon.scope_home),
            rooms = channels.join(","),
            dial = if is_self {
                String::new()
            } else {
                endpoints_for(beacon.peer_id)
            },
            me_tag = if is_self { "   (you)" } else { "" },
        );
    }

    // STALE beacons — older than the freshness window. Usually dead
    // nodes (a stopped daemon, a torn-down container). Default to a
    // one-line summary so the swarm of dead-container ghosts doesn't
    // bury the live set + verdict; `--all` lists every one. `airc
    // registry gc` clears the dead rendezvous gists behind them.
    if !snapshot.stale.is_empty() {
        println!();
        let window_min = DEFAULT_PEER_FRESHNESS_TTL_MS / 60_000;
        if show_all_stale {
            println!(
                "STALE beacons ({}):  (no heartbeat in {window_min}m — likely dead; \
                 `airc registry gc` clears the gists)",
                snapshot.stale.len(),
            );
            for beacon in &snapshot.stale {
                let age = humanize_age_ms(now.saturating_sub(beacon.heartbeat_at_ms));
                println!(
                    "  {peer}  {machine:<9}  rooms=[{rooms}]  {age}",
                    peer = short(&beacon.peer_id.to_string()),
                    machine = machine_label(&beacon.scope_home),
                    rooms = beacon_channels(beacon).join(","),
                );
            }
        } else {
            println!(
                "STALE beacons ({}): {summary}  (no heartbeat in {window_min}m — likely dead; \
                 `airc network --all` lists them, `airc registry gc` clears the gists)",
                snapshot.stale.len(),
                summary = stale_summary(&snapshot.stale),
            );
        }
    }

    // THE KEYSTONE: is anyone actually reachable in the room you send to
    // by default? This is the signal that was missing when BIGMAMA sent
    // into #cambriantech while every Mac was on #general.
    println!();
    let Some(room) = current.as_ref() else {
        // No default room on this scope yet (read-only: we did NOT
        // create one). Nothing to converge against until the operator
        // joins a room.
        println!(
            "\u{2022} no default room on this scope yet — run `airc join` to pick one, \
             then peers in it show as reachable."
        );
        return Ok(());
    };
    let convergence = convergence_status(&room.name, &other_live_channels);
    if convergence.reachable {
        println!(
            "\u{2713} reachable: your default room #{room_name} has live peer(s) in it.",
            room_name = room.name,
        );
    } else if convergence.other_peer_count == 0 {
        println!(
            "\u{2022} no other live peers yet. Run `airc registry sync` here (and on another \
             machine signed into the same GitHub account) to converge."
        );
    } else {
        println!(
            "\u{26a0} CONVERGENCE: your default room #{room_name} has 0 OTHER reachable peers.",
            room_name = room.name,
        );
        println!(
            "   {count} live peer(s) are on: {rooms}",
            count = convergence.other_peer_count,
            rooms = convergence
                .other_rooms
                .iter()
                .map(|r| format!("#{r}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        if let Some(first) = convergence.other_rooms.first() {
            println!(
                "   Reach them: `airc msg --room {first} \"\u{2026}\"`   or switch: \
                 `airc room {first}`",
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routable_lan_filter_rejects_loopback_and_unspecified() {
        assert!(!is_routable_lan_ipv4(Ipv4Addr::LOCALHOST));
        assert!(!is_routable_lan_ipv4(Ipv4Addr::UNSPECIFIED));
        assert!(is_routable_lan_ipv4(Ipv4Addr::new(192, 168, 1, 2)));
    }

    /// BIGMAMA review fix on PR #1201: the 172.18.0.x Docker-bridge
    /// flood was caused by detect_lan_ip publishing whatever the OS
    /// picked as "primary LAN" — including container bridge IPs. The
    /// in_container() probe gates this. Test environment can't easily
    /// simulate /.dockerenv without touching the FS, so we pin the
    /// pure-logic invariant: the filter-as-routable predicate stays
    /// the same shape, and in_container() never returns true on a
    /// dev macOS/Linux that has neither /.dockerenv nor a docker
    /// cgroup. (This test running green is itself the negative-case
    /// proof.)
    // what this catches: the container-reachability seam — an explicit
    // AIRC_ADVERTISE_IP handoff is advertised verbatim, BEFORE (and
    // instead of) host auto-detection. Without this a containerized node
    // (detect_lan_ip → None) advertises no endpoint and never converges.
    #[test]
    fn advertise_ip_env_override_is_honored() {
        temp_env::with_var("AIRC_ADVERTISE_IP", Some("10.0.0.5"), || {
            assert_eq!(advertise_lan_ip(), Some(Ipv4Addr::new(10, 0, 0, 5)));
        });
    }

    // what this catches: a SET-but-garbage override is NOT advertised as a
    // bogus endpoint (no silent fallthrough to a malformed addr); it is
    // ignored so the caller auto-detects. Pins env_advertise_ip directly
    // to stay hermetic (advertise_lan_ip's fallback hits the network).
    #[test]
    fn advertise_ip_invalid_or_blank_is_ignored() {
        temp_env::with_var("AIRC_ADVERTISE_IP", Some("not-an-ip"), || {
            assert_eq!(env_advertise_ip(), None);
        });
        temp_env::with_var("AIRC_ADVERTISE_IP", Some("   "), || {
            assert_eq!(env_advertise_ip(), None);
        });
    }

    #[test]
    fn in_container_returns_false_on_real_host() {
        // The CI runners + dev hosts this test runs on are not OCI
        // containers; the probe MUST return false so detect_lan_ip
        // keeps publishing the real LAN.
        assert!(
            !in_container(),
            "test environment must not be detected as a container; \
             if this fails on a real CI runner that IS a container, \
             revisit the probe before changing the assertion"
        );
    }

    #[test]
    fn tailscale_filter_matches_cgnat_range_only() {
        // In-range (100.64.0.0/10)
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 79, 156, 3)));
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 64, 0, 0)));
        assert!(is_tailscale_ipv4(Ipv4Addr::new(100, 127, 255, 255)));
        // Out of range
        assert!(!is_tailscale_ipv4(Ipv4Addr::new(100, 63, 0, 1))); // below
        assert!(!is_tailscale_ipv4(Ipv4Addr::new(100, 128, 0, 1))); // above
        assert!(!is_tailscale_ipv4(Ipv4Addr::new(192, 168, 1, 2))); // LAN
    }

    // `airc network` convergence + render-helper tests live in the same
    // mod (one test mod per file). The convergence verdict is the
    // keystone — these pin the exact failure this command exists to
    // surface.

    // what this catches: the exact failure that motivated this command —
    // BIGMAMA on #cambriantech, peers on #general. Convergence must
    // report unreachable AND point at the room the peers are actually in.
    // Mutation check: hard-coding `reachable = true` fails here.
    #[test]
    fn convergence_unreachable_when_peers_are_in_a_different_room() {
        let status = convergence_status(
            "cambriantech",
            &[vec!["general".to_string()], vec!["general".to_string()]],
        );
        assert!(!status.reachable, "no peer is in #cambriantech");
        assert_eq!(status.other_peer_count, 2);
        assert_eq!(status.other_rooms, vec!["general".to_string()]);
    }

    // what this catches: a peer sharing the default room flips the
    // verdict to reachable. Mutation check: dropping the `== my_room`
    // match (always-false) fails this.
    #[test]
    fn convergence_reachable_when_a_peer_shares_the_default_room() {
        let status = convergence_status(
            "general",
            &[
                vec!["general".to_string(), "cambriantech".to_string()],
                vec!["academy".to_string()],
            ],
        );
        assert!(status.reachable);
    }

    // what this catches: a lone node (no other live peers) must NOT
    // render the alarmist "0 OTHER reachable peers" path — the caller
    // branches on other_peer_count == 0 for the gentler "sync to
    // converge" hint. Pins that the count is honest.
    #[test]
    fn convergence_lone_node_reports_zero_other_peers_not_a_mismatch() {
        let status = convergence_status("general", &[]);
        assert!(!status.reachable);
        assert_eq!(status.other_peer_count, 0);
        assert!(status.other_rooms.is_empty());
    }

    // what this catches: the "they're over there" hint must be sorted and
    // de-duplicated so the message is stable and readable when many peers
    // span several rooms.
    #[test]
    fn convergence_other_rooms_are_sorted_and_deduped() {
        let status = convergence_status(
            "cambriantech",
            &[
                vec!["general".to_string(), "academy".to_string()],
                vec!["general".to_string()],
                vec!["zeta".to_string()],
            ],
        );
        assert!(!status.reachable);
        assert_eq!(
            status.other_rooms,
            vec![
                "academy".to_string(),
                "general".to_string(),
                "zeta".to_string()
            ]
        );
    }

    // what this catches: the path-shape → machine-label heuristic for the
    // three real fleet shapes seen on the live rendezvous (Windows
    // \\?\C:\, macOS /Users/, container /node/). Cosmetic, but the wrong
    // label sends a debugger down the wrong machine.
    #[test]
    fn machine_label_recognizes_fleet_path_shapes() {
        assert_eq!(
            machine_label(Path::new("\\\\?\\C:\\Users\\joelt\\.airc")),
            "windows"
        );
        assert_eq!(machine_label(Path::new("/Users/joel/.airc")), "macos");
        assert_eq!(machine_label(Path::new("/node/.airc")), "container");
        assert_eq!(machine_label(Path::new("/home/dev/.airc")), "linux");
    }

    // what this catches: the stale summary groups by machine label and
    // orders by descending count (dominant ghost class first), so the
    // default one-line view stays readable when 40+ dead containers
    // pile up. Mutation check: dropping the count-descending sort
    // surfaces a label-alphabetical order and fails the assert.
    #[test]
    fn stale_summary_groups_by_label_count_descending() {
        use airc_lib::{ChannelName, PresenceBeacon};
        use std::path::PathBuf;
        let beacon = |id: u128, home: &str| PresenceBeacon {
            version: 1,
            peer_id: airc_lib::PeerId(uuid::Uuid::from_u128(id)),
            scope_home: PathBuf::from(home),
            subscribed_channels: vec![ChannelName::new("general").unwrap()],
            pid: 1,
            published_at_ms: 0,
            heartbeat_at_ms: 0,
        };
        let stale = vec![
            beacon(1, "/node/.airc"),
            beacon(2, "/node/.airc"),
            beacon(3, "/node/.airc"),
            beacon(4, "/Users/joel/.airc"),
            beacon(5, "\\\\?\\C:\\Users\\joelt\\.airc"),
        ];
        assert_eq!(stale_summary(&stale), "3 container, 1 macos, 1 windows");
    }

    // what this catches: age buckets at the boundaries — a drift here
    // (off-by-one on the 60s/3600s/86400s rollover) misreports liveness.
    #[test]
    fn humanize_age_buckets_at_boundaries() {
        assert_eq!(humanize_age_ms(5_000), "5s ago");
        assert_eq!(humanize_age_ms(59_000), "59s ago");
        assert_eq!(humanize_age_ms(60_000), "1m ago");
        assert_eq!(humanize_age_ms(3_600_000), "1h ago");
        assert_eq!(humanize_age_ms(172_800_000), "2d ago");
    }
}
