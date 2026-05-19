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
    /// Start a subscription on `wire` if one isn't already running.
    /// Daemon buffers received frames into an in-memory inbox per
    /// wire. Idempotent — repeated calls return Ok without
    /// duplicating subscriptions.
    Subscribe(SubscribeRequest),
    /// Read buffered frames from the daemon's inbox for `wire`.
    /// Returns frames strictly after `since_lamport` (if provided),
    /// up to `limit`. Pass back the response's newest_lamport on the
    /// next call to keep the stream "consume-once".
    Inbox(InboxRequest),
    /// Graceful shutdown. Daemon completes in-flight requests, then
    /// stops accepting new connections + exits.
    Stop,
}

/// Parameters for `Subscribe`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscribeRequest {
    /// Wire directory to subscribe on (creates the local-fs adapter
    /// + replay-anchored subscription if not already running).
    pub wire: std::path::PathBuf,
}

/// Parameters for `Inbox`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxRequest {
    /// Wire directory the daemon should pull buffered frames from.
    pub wire: std::path::PathBuf,
    /// Return only frames whose lamport > this value. `None` means
    /// "everything in the buffer."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_lamport: Option<u64>,
    /// Max frames to return in this batch. `None` defaults to a
    /// reasonable cap (32) so a slow client doesn't pull megabytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
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
