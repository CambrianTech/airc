//! Daemon → client responses. Symmetric to `request.rs` — typed
//! enum, wire-tagged by `kind`.

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

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
    /// One live event emitted by an `Attach` stream.
    Event {
        event: Box<airc_core::TranscriptEvent>,
    },
    /// Response to `ListPeers` — the daemon's currently-enrolled
    /// peers (peer_id + URL-safe-no-padding base64 pubkey).
    Peers(PeersResponse),
    /// Generic success for ops that don't return data (`Send`,
    /// `Subscribe`, `AddPeer`, `Stop`).
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

/// One entry in the `Peers` response. Mirrors `peers_store::StoredPeer`
/// but lives in `ipc` so the client doesn't need to depend on the
/// daemon's storage module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerEntry {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
}

/// Snapshot of enrolled peers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeersResponse {
    pub peers: Vec<PeerEntry>,
}

/// Result of an `Inbox` pull: events + the cursor to feed back as
/// `since` on the next call. Returned events are oldest → newest
/// within the page.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxResponse {
    /// Up to `limit` events matching the request, in transcript
    /// order `(lamport asc, event_id asc)`.
    pub events: Vec<airc_core::TranscriptEvent>,
    /// Cursor of the newest event in `events`. `None` when the page
    /// was empty — in that case the caller's `since` is still the
    /// authoritative position to feed back on the next poll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<airc_core::TranscriptCursor>,
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

    #[test]
    fn event_response_wraps_transcript_without_tag_collision() {
        use airc_core::{
            Body, ClientId, EventId, Headers, MentionTarget, RoomId, TranscriptEvent,
            TranscriptKind,
        };

        let response = Response::Event {
            event: Box::new(TranscriptEvent {
                event_id: EventId::new(),
                room_id: RoomId::new(),
                peer_id: PeerId::new(),
                client_id: ClientId::new(),
                kind: TranscriptKind::Message,
                occurred_at_ms: 1,
                lamport: 1,
                target: MentionTarget::All,
                headers: Headers::new(),
                body: Some(Body::text("hello")),
                attachment: None,
                receipt: None,
                metadata: serde_json::Value::Null,
            }),
        };

        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, response);
    }
}
