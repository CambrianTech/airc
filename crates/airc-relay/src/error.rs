//! Fail-closed relay errors. No silent degradation to plaintext or
//! to an unknown-peer fallback.

use std::io;

use airc_transport::lan_tcp::TlsConfigError;

#[derive(Debug, thiserror::Error)]
pub enum RelayServerError {
    #[error("relay TLS configuration failed: {0}")]
    Tls(#[from] TlsConfigError),

    #[error("relay I/O failed: {0}")]
    Io(#[from] io::Error),

    #[error("relay was asked to listen twice — only one accept loop is supported")]
    AlreadyListening,

    #[error("connecting peer's certificate could not be bound to an Ed25519 pubkey")]
    PeerCertNotEd25519,

    #[error("frame on the wire exceeded the per-frame size cap of {limit} bytes")]
    FrameTooLarge { limit: u32 },

    #[error("frame JSON decode failed: {0}")]
    FrameDecode(#[from] serde_json::Error),

    #[error("connection closed before frame body could be read")]
    UnexpectedEof,
}
