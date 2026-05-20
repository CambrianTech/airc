//! Fail-closed relay-client errors. No silent fall-back to plaintext.

use std::io;

use crate::lan_tcp::TlsConfigError;

#[derive(Debug)]
pub enum RelayClientError {
    Tls(TlsConfigError),
    Io(io::Error),
    Json(serde_json::Error),
    /// `Transport::send` was called before [`super::RelayAdapter::connect`].
    NotConnected,
    /// `RelayAdapter::connect` was called twice on the same adapter.
    AlreadyConnected,
    /// Frame bytes exceeded the per-frame wire limit.
    FrameTooLarge {
        actual: usize,
        limit: u32,
    },
    /// Outbound channel closed mid-send — relay went away or the
    /// connection task exited.
    ConnectionClosed,
    /// Relay certificate server name could not be constructed from
    /// the relay peer id.
    InvalidServerName(String),
}

impl std::fmt::Display for RelayClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tls(e) => write!(f, "relay client TLS config build failed: {e}"),
            Self::Io(e) => write!(f, "relay client I/O failed: {e}"),
            Self::Json(e) => write!(f, "frame serialization failed: {e}"),
            Self::NotConnected => f.write_str("relay client tried to send before connect()"),
            Self::AlreadyConnected => f.write_str("relay client tried to connect() twice"),
            Self::FrameTooLarge { actual, limit } => {
                write!(f, "frame size {actual} exceeds per-frame limit {limit}")
            }
            Self::ConnectionClosed => f.write_str("relay connection closed by remote"),
            Self::InvalidServerName(name) => {
                write!(f, "relay server name is invalid: {name}")
            }
        }
    }
}

impl std::error::Error for RelayClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Tls(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::NotConnected
            | Self::AlreadyConnected
            | Self::FrameTooLarge { .. }
            | Self::ConnectionClosed
            | Self::InvalidServerName(_) => None,
        }
    }
}

impl From<TlsConfigError> for RelayClientError {
    fn from(e: TlsConfigError) -> Self {
        Self::Tls(e)
    }
}
impl From<io::Error> for RelayClientError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for RelayClientError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
