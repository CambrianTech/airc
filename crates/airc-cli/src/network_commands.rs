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
    socket.connect((Ipv4Addr::new(100, 100, 100, 100), 80)).ok()?;
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
