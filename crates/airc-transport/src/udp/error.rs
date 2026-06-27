use airc_core::PeerId;
use airc_protocol::FrameKind;

/// UDP transport errors.
#[derive(Debug)]
pub enum UdpError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// UDP datagrams are bounded. The adapter refuses oversized
    /// frames before send instead of truncating or fragmenting.
    FrameTooLarge {
        actual: usize,
        limit: usize,
    },
    /// UDP is not a durable transcript route.
    UnsupportedDurableKind(FrameKind),
    /// No endpoint is known for a direct peer target.
    UnknownPeerEndpoint(PeerId),
    /// Broadcast send requested with no configured peer endpoints.
    NoPeerEndpoints,
    /// `bind()` called twice on one adapter.
    AlreadyBound,
    /// `send()`/`subscribe()` called before `bind()`.
    NotBound,
}

impl std::fmt::Display for UdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "udp I/O: {error}"),
            Self::Json(error) => write!(f, "udp frame parse: {error}"),
            Self::FrameTooLarge { actual, limit } => {
                write!(f, "udp frame size {actual} exceeds datagram limit {limit}")
            }
            Self::UnsupportedDurableKind(kind) => {
                write!(f, "udp does not support durable frame kind {kind:?}")
            }
            Self::UnknownPeerEndpoint(peer) => {
                write!(f, "udp has no endpoint for peer {peer}")
            }
            Self::NoPeerEndpoints => f.write_str("udp has no configured peer endpoints"),
            Self::AlreadyBound => f.write_str("udp adapter is already bound"),
            Self::NotBound => f.write_str("udp adapter is not bound"),
        }
    }
}

impl std::error::Error for UdpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::FrameTooLarge { .. }
            | Self::UnsupportedDurableKind(_)
            | Self::UnknownPeerEndpoint(_)
            | Self::NoPeerEndpoints
            | Self::AlreadyBound
            | Self::NotBound => None,
        }
    }
}

impl From<std::io::Error> for UdpError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for UdpError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
