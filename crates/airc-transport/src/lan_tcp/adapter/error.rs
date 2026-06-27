//! `LanTcpError` — the LAN adapter's typed failure surface.

use airc_core::PeerId;

use crate::lan_tcp::tls_config::TlsConfigError;

/// LAN transport errors.
#[derive(Debug)]
pub enum LanTcpError {
    Io(std::io::Error),
    Json(serde_json::Error),
    TlsConfig(TlsConfigError),
    TlsHandshake(std::io::Error),
    /// Post-handshake peer-id binding failed: cert presented didn't
    /// resolve to a known peer.
    PeerNotInRegistry,
    /// Length prefix exceeded the per-frame size cap — likely hostile
    /// or misconfigured.
    FrameTooLarge {
        announced: u32,
        limit: u32,
    },
    /// `send()` called with no peers connected.
    NoActivePeers,
    /// `connect()` called for a peer that's already connected on this
    /// adapter. (Listening sides don't see this; they only get new
    /// peers via accept.)
    AlreadyConnectedTo(PeerId),
    /// `listen()` called more than once on the same adapter — only
    /// one bound listener supported.
    AlreadyListening,
}

impl std::fmt::Display for LanTcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LanTcpError::Io(error) => write!(f, "lan-tcp I/O: {error}"),
            LanTcpError::Json(error) => write!(f, "lan-tcp frame parse: {error}"),
            LanTcpError::TlsConfig(error) => write!(f, "lan-tcp TLS config: {error}"),
            LanTcpError::TlsHandshake(error) => write!(f, "lan-tcp TLS handshake: {error}"),
            LanTcpError::PeerNotInRegistry => write!(
                f,
                "post-handshake: peer cert pubkey is not in the registry (this should have been caught at handshake)"
            ),
            LanTcpError::FrameTooLarge { announced, limit } => write!(
                f,
                "lan-tcp refused frame with announced size {announced} bytes (limit {limit})"
            ),
            LanTcpError::NoActivePeers => write!(
                f,
                "lan-tcp adapter has no connected peers — call listen() or connect() first"
            ),
            LanTcpError::AlreadyConnectedTo(peer) => {
                write!(f, "lan-tcp adapter is already connected to peer {peer}")
            }
            LanTcpError::AlreadyListening => {
                write!(f, "lan-tcp adapter already has a bound listener")
            }
        }
    }
}

impl std::error::Error for LanTcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LanTcpError::Io(error) | LanTcpError::TlsHandshake(error) => Some(error),
            LanTcpError::Json(error) => Some(error),
            LanTcpError::TlsConfig(error) => Some(error),
            LanTcpError::PeerNotInRegistry
            | LanTcpError::FrameTooLarge { .. }
            | LanTcpError::NoActivePeers
            | LanTcpError::AlreadyConnectedTo(_)
            | LanTcpError::AlreadyListening => None,
        }
    }
}

impl From<std::io::Error> for LanTcpError {
    fn from(error: std::io::Error) -> Self {
        LanTcpError::Io(error)
    }
}

impl From<serde_json::Error> for LanTcpError {
    fn from(error: serde_json::Error) -> Self {
        LanTcpError::Json(error)
    }
}

impl From<TlsConfigError> for LanTcpError {
    fn from(error: TlsConfigError) -> Self {
        LanTcpError::TlsConfig(error)
    }
}
