//! Transport-level errors.
//!
//! Each adapter declares its own `Error` associated type so callers can
//! discriminate failure modes per adapter without a one-size-fits-all
//! enum. This module hosts the local-fs adapter's error type; the
//! lan-tcp adapter's error lives alongside that adapter in
//! `lan_tcp/adapter.rs`.

use std::fmt;

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
