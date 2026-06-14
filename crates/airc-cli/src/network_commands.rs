use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

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
}
