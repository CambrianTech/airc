//! Client → daemon requests. Add new operations by extending the
//! `Request` enum; the daemon's dispatcher is exhaustiveness-checked
//! so the compiler nags you to handle every variant.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single client-issued operation. Wire-tagged by `op`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Liveness probe. Returns `Response::Pong`.
    Ping,
    /// Snapshot of daemon state (peer id, uptime, …).
    Status,
    /// Send a text Message frame. Daemon signs + dispatches via its
    /// owned `SignedTransport`.
    Send(SendRequest),
    /// Graceful shutdown. Daemon completes in-flight requests, then
    /// stops accepting new connections + exits.
    Stop,
}

/// Parameters for `Send`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendRequest {
    /// Wire directory the daemon should write to.
    pub wire: PathBuf,
    /// Channel UUID. Use a stable value across peers in the same room.
    pub channel: Uuid,
    /// Body text.
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_serializes_compactly() {
        // The simplest variant — wire-tag only. Pinned so we catch
        // accidental unwrapping (e.g. adding fields by mistake).
        let encoded = serde_json::to_string(&Request::Ping).unwrap();
        assert_eq!(encoded, r#"{"op":"ping"}"#);
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, Request::Ping);
    }

    #[test]
    fn send_roundtrips_with_typed_fields() {
        let original = Request::Send(SendRequest {
            wire: PathBuf::from("/tmp/wire"),
            channel: Uuid::nil(),
            text: "hello".to_string(),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn stop_serializes_compactly() {
        assert_eq!(
            serde_json::to_string(&Request::Stop).unwrap(),
            r#"{"op":"stop"}"#
        );
    }
}
