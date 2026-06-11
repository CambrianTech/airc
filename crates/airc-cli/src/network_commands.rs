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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routable_lan_filter_rejects_loopback_and_unspecified() {
        assert!(!is_routable_lan_ipv4(Ipv4Addr::LOCALHOST));
        assert!(!is_routable_lan_ipv4(Ipv4Addr::UNSPECIFIED));
        assert!(is_routable_lan_ipv4(Ipv4Addr::new(192, 168, 1, 2)));
    }
}
