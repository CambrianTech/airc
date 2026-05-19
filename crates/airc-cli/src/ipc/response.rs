//! Daemon → client responses. Symmetric to `request.rs` — typed
//! enum, wire-tagged by `kind`.

use serde::{Deserialize, Serialize};

use airc_protocol::Frame;

/// One response to a `Request`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Response to `Ping`.
    Pong,
    /// Response to `Status`.
    Status(StatusResponse),
    /// Response to `Inbox` — buffered frames + a "newest cursor" the
    /// caller threads back on the next call to keep the stream
    /// consume-once.
    Inbox(InboxResponse),
    /// Generic success for ops that don't return data (`Send`,
    /// `Subscribe`, `Stop`).
    Ok,
    /// Failure — typed message so the client can render it.
    Error { message: String },
}

/// Daemon health/state snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusResponse {
    /// Peer UUID as the hyphenated string form.
    pub peer_id: String,
    /// Seconds since daemon start.
    pub uptime_seconds: u64,
}

/// Result of an `Inbox` pull: frames + the lamport to feed back as
/// `since_lamport` on the next call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxResponse {
    /// Up to `limit` frames matching the request, lamport-ascending.
    pub frames: Vec<Frame>,
    /// Largest lamport in `frames`, or `since_lamport` echoed back if
    /// the call returned no frames. Pass this on the next call.
    pub newest_lamport: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pong_serializes_compactly() {
        assert_eq!(
            serde_json::to_string(&Response::Pong).unwrap(),
            r#"{"kind":"pong"}"#
        );
    }

    #[test]
    fn status_roundtrips() {
        let original = Response::Status(StatusResponse {
            peer_id: "07e7ad58-ba56-4535-b4e5-a161a110e487".to_string(),
            uptime_seconds: 42,
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn error_carries_message() {
        let error = Response::Error {
            message: "boom".to_string(),
        };
        let encoded = serde_json::to_string(&error).unwrap();
        assert!(encoded.contains("boom"));
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, error);
    }
}
