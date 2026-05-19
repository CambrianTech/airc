//! Transport-level errors.
//!
//! Each adapter declares its own `Error` associated type, but they share
//! the same general failure modes (I/O failed, serialization failed,
//! the wire isn't ready). This module gives `LocalFsAdapter` a typed
//! error; other adapters will follow the same pattern.

use std::fmt;

/// What can go wrong inside the lan-tcp adapter.
///
/// LAN-TCP is **DEV-PREVIEW** until PR-4 wires rustls peer pinning +
/// real Ed25519 verification. The error variants below include the
/// security-guard refusal cases so the substrate fails closed rather
/// than silently exposing a plaintext wire on the LAN.
#[derive(Debug)]
pub enum LanTcpError {
    /// File or socket I/O failed.
    Io(std::io::Error),

    /// A peer sent bytes that didn't parse as a `Frame`. The substrate
    /// refuses rather than silently dropping — possibly a protocol
    /// mismatch, a hostile peer, or a wire corruption.
    Json(serde_json::Error),

    /// The caller asked to bind a non-loopback address without
    /// threading `unsafe_allow_public_bind()`. Default refuses — the
    /// LAN adapter is DEV-PREVIEW and must not be exposed on a real
    /// network without explicit opt-in.
    PublicBindNotAllowed(std::net::SocketAddr),

    /// The builder was driven to `.build()` without a bind address.
    MissingBindAddr,

    /// A peer sent a length-prefix exceeding the per-frame limit.
    /// Substrate caps frame size to defend against OOM from a hostile
    /// peer. Honest senders stay well under via the body-lift policy.
    FrameTooLarge { announced: u64, limit: u64 },
}

impl fmt::Display for LanTcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LanTcpError::Io(error) => write!(f, "lan-tcp transport I/O: {error}"),
            LanTcpError::Json(error) => write!(f, "lan-tcp transport frame parse: {error}"),
            LanTcpError::PublicBindNotAllowed(addr) => write!(
                f,
                "lan-tcp refused to bind non-loopback address {addr} \
                 — adapter is DEV-PREVIEW (no transport encryption, \
                 no Ed25519 verification wired); thread \
                 unsafe_allow_public_bind() to override"
            ),
            LanTcpError::MissingBindAddr => write!(
                f,
                "lan-tcp builder missing bind address — call .bind(addr) before .build()"
            ),
            LanTcpError::FrameTooLarge { announced, limit } => write!(
                f,
                "lan-tcp refused frame with announced size {announced} bytes \
                 (limit {limit}) — bodies above limit must be lifted to \
                 airc-blobs and carried as MediaRef"
            ),
        }
    }
}

impl std::error::Error for LanTcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LanTcpError::Io(error) => Some(error),
            LanTcpError::Json(error) => Some(error),
            _ => None,
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

/// What can go wrong inside the local-fs adapter.
#[derive(Debug)]
pub enum LocalFsError {
    /// File or directory I/O failed — couldn't open the wire dir,
    /// couldn't read the frames file, fsync failed, etc.
    Io(std::io::Error),

    /// A line in the frames file failed to parse as a `Frame`. Either
    /// the file was corrupted, or a different writer is using an
    /// incompatible format on the same wire. The substrate refuses
    /// rather than silently dropping — the caller decides whether to
    /// continue or abort.
    Json(serde_json::Error),
}

impl fmt::Display for LocalFsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalFsError::Io(error) => write!(f, "local-fs transport I/O: {error}"),
            LocalFsError::Json(error) => write!(f, "local-fs transport frame parse: {error}"),
        }
    }
}

impl std::error::Error for LocalFsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LocalFsError::Io(error) => Some(error),
            LocalFsError::Json(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for LocalFsError {
    fn from(error: std::io::Error) -> Self {
        LocalFsError::Io(error)
    }
}

impl From<serde_json::Error> for LocalFsError {
    fn from(error: serde_json::Error) -> Self {
        LocalFsError::Json(error)
    }
}
