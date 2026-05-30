//! Daemon → client responses. Symmetric to `request.rs` — typed
//! enum, wire-tagged by `kind`.
//!
//! Owner-core model: live events and inbox pages cross the IPC boundary
//! as **opaque airc-wire bytes** (`airc_wire::encode(&Envelope)`) — the
//! daemon encodes once, the client decodes once. The IPC layer stays
//! ignorant of the envelope's shape (no `airc-bus` dependency leaks
//! here, no per-hop re-serialize).

use serde::{Deserialize, Serialize};

use airc_core::{EventId, PeerId, RoomId};

use crate::request::IpcCursor;

/// One response to a `Request`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Response to `Ping`.
    Pong,
    /// Response to `Status`.
    Status(StatusResponse),
    /// Response to `Inbox` — durable envelopes (airc-wire bytes) + a
    /// "newest cursor" the caller threads back on the next call to keep
    /// the stream consume-once.
    Inbox(InboxResponse),
    /// One live event emitted by an `Attach` stream — the airc-wire
    /// encoding of the bus `Envelope`. The client decodes via
    /// `airc_wire::decode`.
    Event { envelope: Vec<u8> },
    /// **Card 7d5b6a65.** Emitted by an `Attach` stream when the
    /// client requested `coalesce_backlog: true` and the daemon had
    /// backlog to catch up on. ONE summary frame per attach catch-up,
    /// then the stream transitions to live tail; each subsequent live
    /// event arrives as its own `Event` frame.
    ///
    /// `skipped` is the count of historical events the daemon
    /// suppressed during catch-up. `advanced_to` is the cursor the
    /// daemon resumed from — the client may persist this and pass it
    /// as `from` on a future reconnect to skip the same backlog
    /// without `from_now`. When `skipped: 0`, the catch-up phase was
    /// empty (no backlog at attach time) and the frame is suppressed
    /// — the daemon emits this variant only when it actually omitted
    /// at least one event.
    AttachCursorAdvanced {
        skipped: u64,
        advanced_to: IpcCursor,
    },
    /// Response to `Publish` / `Send` — the owner-assigned receipt.
    Publish(PublishResponse),
    /// Response to `ListPeers` — the daemon's currently-enrolled
    /// peers (peer_id + URL-safe-no-padding base64 pubkey).
    Peers(PeersResponse),
    /// Generic success for ops that don't return data (`AddPeer`,
    /// `RemovePeer`, `Stop`, and the initial `Attach` ack).
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
    /// IPC protocol version spoken by this daemon. Missing means the
    /// daemon predates status metadata and should be treated as stale
    /// by lifecycle code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipc_protocol_version: Option<u32>,
    /// Build commit baked into the daemon binary. Missing means
    /// unknown/old daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,
    /// Build branch baked into the daemon binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_branch: Option<String>,
    /// Executable path of the daemon process. This is diagnostics
    /// only; lifecycle decisions use protocol + build metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
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

/// Result of an `Inbox` pull: durable envelopes (airc-wire bytes) + the
/// cursor to feed back as `since` on the next call. Envelopes are in
/// total order `(epoch, counter, event_id)`, oldest → newest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxResponse {
    /// Up to `limit` envelopes matching the request, each an
    /// `airc_wire::encode(&Envelope)` buffer.
    pub envelopes: Vec<Vec<u8>>,
    /// Cursor of the newest envelope in `envelopes`. `None` when the
    /// page was empty — the caller's `since` stays authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<IpcCursor>,
}

/// Owner-assigned receipt returned by `Send` / `Publish`. The
/// `(epoch, counter)` seq IS the authoritative total order; wall-clock
/// `occurred_at` lives on the envelope itself (decode from inbox/attach
/// bytes if a client needs it), so it isn't duplicated here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishResponse {
    pub event_id: EventId,
    /// Generational epoch of the assigned `(epoch, counter)` seq.
    pub epoch: u64,
    /// Monotonic counter within the epoch.
    pub counter: u64,
    /// Owner-stamped wall-clock at publish (informational; the
    /// authoritative order is `(epoch, counter)`).
    pub occurred_at_ms: u64,
    pub channel_id: RoomId,
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
            ipc_protocol_version: Some(3),
            build_commit: Some("abc123".to_string()),
            build_branch: Some("rust-rewrite".to_string()),
            executable: Some("/tmp/airc".to_string()),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn status_accepts_pre_metadata_daemon_response() {
        let decoded: Response = serde_json::from_str(
            r#"{"kind":"status","peer_id":"07e7ad58-ba56-4535-b4e5-a161a110e487","uptime_seconds":42}"#,
        )
        .unwrap();
        assert_eq!(
            decoded,
            Response::Status(StatusResponse {
                peer_id: "07e7ad58-ba56-4535-b4e5-a161a110e487".to_string(),
                uptime_seconds: 42,
                ipc_protocol_version: None,
                build_commit: None,
                build_branch: None,
                executable: None,
            })
        );
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
    fn publish_response_roundtrips_with_epoch_counter() {
        let original = Response::Publish(PublishResponse {
            event_id: EventId::from_u128(1),
            epoch: 2,
            counter: 9,
            occurred_at_ms: 1_700_000_000_000,
            channel_id: RoomId::from_u128(4),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn inbox_response_roundtrips_with_wire_bytes() {
        let original = Response::Inbox(InboxResponse {
            envelopes: vec![vec![1, 2, 3], vec![4, 5, 6, 7]],
            newest: Some(IpcCursor {
                epoch: 1,
                counter: 2,
                event_id: EventId::from_u128(3),
            }),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn event_response_carries_opaque_wire_bytes() {
        let response = Response::Event {
            envelope: vec![0xa, 0xb, 0xc],
        };
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, response);
    }
}
