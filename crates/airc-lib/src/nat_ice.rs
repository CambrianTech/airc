//! ICE configuration for WebRTC NAT traversal — the free coffee-shop rung.
//!
//! ## Why this exists
//!
//! `webrtc.rs` shipped with the loopback follow-up open: PeerConnections
//! bound `127.0.0.1:0` with no ICE servers, so the DataChannel path only
//! worked between two endpoints on the same machine. This module closes
//! it: real-interface binds + STUN servers give ICE server-reflexive
//! candidates, which is standard UDP hole-punching — the majority of
//! NAT'd peer pairs (home router ↔ coffee-shop wifi) connect DIRECTLY,
//! no subscription, no VPN. Pairs that can't punch (symmetric↔symmetric)
//! keep the relay rung; the relay only ever carries what can't go direct.
//!
//! ## Defaults, and who pays
//!
//! Nobody. STUN is a stateless "what's my public address:port" echo —
//! several operators run free public servers precisely because it costs
//! them almost nothing. We default to a small redundant set and let
//! operators point at their own (`AIRC_STUN_SERVERS`), including a grid
//! member's future STUN-speaking relay. TURN-style relaying is NOT
//! configured here — bulk fallback belongs to the grid's own relay rung,
//! not a third party's.
//!
//! ## Env overrides
//!
//! - `AIRC_STUN_SERVERS` — comma-separated `stun:host:port` URLs. Empty
//!   string means "no STUN" (host candidates only — LAN/loopback use).
//! - `AIRC_WEBRTC_BIND` — UDP bind address for ICE gathering. Defaults
//!   to `0.0.0.0:0` (all interfaces, OS-assigned port).
//!
//! Tests use [`IceConfig::loopback_only`] explicitly — never env
//! mutation, which races parallel tests.

use webrtc::peer_connection::{RTCConfiguration, RTCConfigurationBuilder, RTCIceServer};

/// Default public STUN set. Two independent operators for redundancy;
/// both are long-lived, free, and rate-benign for our call volume (one
/// query pair per ICE gather, not per frame).
const DEFAULT_STUN_URLS: &[&str] = &[
    "stun:stun.l.google.com:19302",
    "stun:stun.cloudflare.com:3478",
];

/// Default gather bind: every interface, OS-assigned port.
const DEFAULT_BIND: &str = "0.0.0.0:0";

/// Loopback gather bind — same-machine only (the pre-NAT-traversal
/// behavior, kept for tests and explicitly-local embeddings).
const LOOPBACK_BIND: &str = "127.0.0.1:0";

/// How a PeerConnection gathers ICE candidates: where to bind, and
/// which STUN servers (if any) to learn public reflexive addresses from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceConfig {
    /// `stun:host:port` URLs. Empty = host candidates only.
    pub stun_urls: Vec<String>,
    /// UDP address the ICE agent binds for gathering.
    pub udp_bind: String,
}

impl IceConfig {
    /// Production resolution: env overrides, else real-interface bind +
    /// the default public STUN set.
    pub fn from_env() -> Self {
        Self::from_parts(
            std::env::var("AIRC_STUN_SERVERS").ok().as_deref(),
            std::env::var("AIRC_WEBRTC_BIND").ok().as_deref(),
        )
    }

    /// The pure core of [`from_env`] — testable without env mutation.
    /// `stun`: `None` = default set; `Some("")` = explicitly none;
    /// `Some("a,b")` = exactly those. `bind`: `None`/empty = default.
    pub fn from_parts(stun: Option<&str>, bind: Option<&str>) -> Self {
        let stun_urls = match stun {
            None => DEFAULT_STUN_URLS.iter().map(|s| s.to_string()).collect(),
            Some(raw) => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        };
        let udp_bind = match bind {
            None => DEFAULT_BIND.to_string(),
            Some(raw) if raw.trim().is_empty() => DEFAULT_BIND.to_string(),
            Some(raw) => raw.trim().to_string(),
        };
        Self {
            stun_urls,
            udp_bind,
        }
    }

    /// Same-machine-only gathering: loopback bind, no STUN. The exact
    /// pre-NAT-traversal behavior — what the webrtc integration tests
    /// pin so they stay hermetic (no STUN round-trips in CI).
    pub fn loopback_only() -> Self {
        Self {
            stun_urls: Vec::new(),
            udp_bind: LOOPBACK_BIND.to_string(),
        }
    }

    /// Project into the webrtc crate's `RTCConfiguration`.
    pub fn rtc_configuration(&self) -> RTCConfiguration {
        let servers = self
            .stun_urls
            .iter()
            .map(|url| RTCIceServer {
                urls: vec![url.clone()],
                username: String::new(),
                credential: String::new(),
            })
            .collect::<Vec<_>>();
        RTCConfigurationBuilder::new()
            .with_ice_servers(servers)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_parts_resolves_defaults_overrides_and_explicit_none() {
        // what this catches: the env contract — None means the public
        // default set (production is NAT-capable by default, never a
        // silent loopback), "" means explicitly no STUN, and a CSV is
        // taken verbatim. A regression here silently re-opens the
        // "works on my LAN, dead at the coffee shop" gap.
        let default = IceConfig::from_parts(None, None);
        assert_eq!(default.stun_urls.len(), DEFAULT_STUN_URLS.len());
        assert_eq!(default.udp_bind, DEFAULT_BIND);

        let none = IceConfig::from_parts(Some(""), None);
        assert!(none.stun_urls.is_empty());

        let custom = IceConfig::from_parts(
            Some(" stun:relay.grid.example:3478 , stun:other:19302 "),
            Some("192.168.1.10:0"),
        );
        assert_eq!(
            custom.stun_urls,
            vec!["stun:relay.grid.example:3478", "stun:other:19302"]
        );
        assert_eq!(custom.udp_bind, "192.168.1.10:0");

        let loopback = IceConfig::loopback_only();
        assert!(loopback.stun_urls.is_empty());
        assert_eq!(loopback.udp_bind, LOOPBACK_BIND);
    }
}
