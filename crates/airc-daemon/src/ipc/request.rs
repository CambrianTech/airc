//! Client → daemon requests. Add new operations by extending the
//! `Request` enum; the daemon's dispatcher is exhaustiveness-checked
//! so the compiler nags you to handle every variant.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::{Headers, PeerId};

/// A single client-issued operation. Wire-tagged by `op`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Liveness probe. Returns `Response::Pong`.
    Ping,
    /// Snapshot of daemon state (peer id, uptime, …).
    Status,
    /// Enrol a peer in the daemon's in-memory registry. Durable peer
    /// trust lives in the store; this op keeps the running daemon's
    /// registry in sync without a restart.
    AddPeer(AddPeerRequest),
    /// Snapshot of currently-enrolled peers (peer_id + pubkey).
    /// Returned via `Response::Peers`.
    ListPeers,
    /// Send a text Message frame. Daemon signs + dispatches via its
    /// owned `SignedTransport`.
    Send(SendRequest),
    /// Start a subscription on `wire` if one isn't already running.
    /// Daemon buffers received frames into an in-memory inbox per
    /// wire. Idempotent — repeated calls return Ok without
    /// duplicating subscriptions.
    Subscribe(SubscribeRequest),
    /// Read events from the daemon's durable event store, strictly
    /// after `since` (a `(lamport, event_id)` cursor) and optionally
    /// filtered to a single channel. Pass back the response's
    /// `newest` cursor on the next call for consume-once streaming.
    Inbox(InboxRequest),
    /// Attach to the daemon's live event stream. This is a long-lived
    /// request: after an initial `Response::Ok`, the daemon writes
    /// `Response::Event` lines until the client disconnects.
    Attach(AttachRequest),
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
    /// Return only events strictly after this cursor. `None` means
    /// "give me the most recent events available."
    ///
    /// Cursor is `(lamport, event_id)` per grievance §7 — lamport is
    /// the primary order, event_id is the deterministic tiebreaker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<airc_core::TranscriptCursor>,
    /// Restrict to events on this channel (room). `None` means "any
    /// channel" — used when the caller wants global tail rather than
    /// per-room paging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<airc_core::RoomId>,
    /// Max events to return in this batch. `None` defaults to a
    /// reasonable cap (32) so a slow client doesn't pull megabytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Parameters for `Attach`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AttachRequest {
    /// Restrict live events to this channel. `None` streams all
    /// subscribed daemon events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<airc_core::RoomId>,
}

/// Parameters for `AddPeer`. `pubkey_b64` is the URL-safe-no-padding
/// base64 of the 32-byte Ed25519 pubkey (matches the `peer add <spec>`
/// argument shape on the CLI).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddPeerRequest {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
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
    /// Optional envelope headers supplied by the caller. Used for
    /// runtime consumer metadata such as `airc.client`.
    #[serde(default)]
    pub headers: Headers,
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
            headers: Headers::new(),
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
