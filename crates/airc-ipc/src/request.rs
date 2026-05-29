//! Client → daemon requests. Add new operations by extending the
//! `Request` enum; the daemon's dispatcher is exhaustiveness-checked
//! so the compiler nags you to handle every variant.
//!
//! Owner-core model: there is **no wire**. Same-machine delivery is the
//! daemon's in-memory router fan-out, not a `frames.jsonl` file. A room
//! is addressed by its `channel` UUID; the daemon keys its `EventRouter`
//! on it. There is no `Subscribe` (room drain) and no `ResolveWire`
//! (channel→file lookup) — both were artifacts of the file-wire path.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::{HeaderFilter, Headers, PeerId};

/// How an envelope is delivered + retained — mirrors
/// `airc_bus::DeliveryClass` without leaking the bus type across the
/// IPC boundary. **Only `Durable` reaches the ORM.** Presence/typing
/// (`EphemeralLatest`) and media/game-state chunks (`StreamChunk`) route
/// live, in-memory, zero-copy — never persisted — which is what keeps
/// the high-throughput / low-latency streaming path (games, WebRTC
/// signalling/media) off the durable tier. The daemon maps this to the
/// bus class at publish.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IpcDelivery {
    /// Persisted to the ORM via write-behind; the chat/transcript class.
    #[default]
    Durable,
    /// Latest-wins coalesced in-memory, TTL'd. Presence, pose, typing.
    /// Never an ORM row.
    EphemeralLatest,
    /// Bounded recent-N window, in-memory only.
    EphemeralWindow,
    /// Request leg of a request/response correlation; routed live.
    RequestResponse,
    /// A chunk of a longer stream (media, game-state diffs, progress).
    /// Routed live, not persisted — the zero-copy high-rate path.
    StreamChunk,
}

/// Addressing for a publish — mirrors `airc_bus::envelope::Target`
/// without leaking the bus type. The full vocabulary so every consumer
/// fits: broadcast, direct peer, a named endpoint, a correlation reply
/// leg (RPC), or a capability set the grid-router fans out to (remote
/// inference / foundry, §3.9).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IpcTarget {
    #[default]
    All,
    Peer(PeerId),
    Endpoint(String),
    Reply(Uuid),
    Capability(String),
}

/// Envelope category — mirrors `airc_bus::envelope::Kind` for attach-time
/// filtering, so a consumer can subscribe to just the kinds it handles
/// (e.g. Hermes filters to Command/CommandResult) without the router
/// fanning out everything else.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IpcKind {
    Message,
    Event,
    Command,
    CommandResult,
    Signal,
    StreamChunk,
    Control,
}

/// A cursor into a channel's total order: the owner-assigned
/// `(epoch, counter)` plus the `event_id` tiebreaker. Mirrors
/// `airc_bus::Cursor` without leaking the bus type across the IPC
/// boundary — the daemon maps between them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpcCursor {
    pub epoch: u64,
    pub counter: u64,
    /// The deterministic tiebreaker at one seq. Maps 1:1 to the bus
    /// cursor's `event_id`; serializes as a UUID string.
    pub event_id: airc_core::EventId,
}

/// A single client-issued operation. Wire-tagged by `op`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Remove a peer from the daemon's in-memory registry after the
    /// durable trust store has been updated.
    RemovePeer(RemovePeerRequest),
    /// Snapshot of currently-enrolled peers (peer_id + pubkey).
    /// Returned via `Response::Peers`.
    ListPeers,
    /// Send a text Message on a channel. The daemon publishes it to its
    /// `EventRouter` as a `Durable` envelope and returns a receipt.
    Send(SendRequest),
    /// Publish a structured frame on a channel. The daemon publishes to
    /// its router and returns the `(epoch, counter)` receipt in
    /// `Response::Publish`.
    Publish(PublishRequest),
    /// Read durable events on a channel strictly after `since`, in total
    /// order. Replay comes from the router's hot ring + SQLite durable
    /// tier (no gap at the ring/sink seam). Pass back the response's
    /// `newest` cursor for consume-once paging.
    Inbox(InboxRequest),
    /// Attach to the daemon's live event stream. Long-lived: after an
    /// initial `Response::Ok`, the daemon streams `Response::Event`
    /// frames (airc-wire bytes) until the client disconnects. Optionally
    /// resumes from `from` so replay→live has no gap and no dup.
    Attach(AttachRequest),
    /// Graceful shutdown. Daemon completes in-flight requests, then
    /// stops accepting new connections + exits.
    Stop,
}

/// Parameters for `Inbox`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxRequest {
    /// Return only events strictly after this cursor. `None` means
    /// "give me the most recent events available."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<IpcCursor>,
    /// Restrict to events on this channel (room). `None` means "any
    /// channel" — a global tail rather than per-room paging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<airc_core::RoomId>,
    /// Max events to return in this batch. `None` defaults to a
    /// reasonable cap (32) so a slow client doesn't pull megabytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Parameters for `Attach`. Filters are applied **router-side** so the
/// daemon never fans out events the consumer would discard — the
/// adaptable/performant subscription: Hermes attaches to
/// Command/CommandResult, Continuum scopes by `forge.continuum.*`
/// headers, a game attaches to one room's StreamChunk only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AttachRequest {
    /// The channel (room) to attach to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<airc_core::RoomId>,
    /// Resume strictly after this cursor before going live — replay the
    /// gap the client missed while detached, then continue live with no
    /// duplicate at the seam.
    ///
    /// `None` historically meant "give me everything from the beginning
    /// of the transcript" (despite the prior docstring claiming "live
    /// edge"). Card 7d5b6a65 splits intent: pair `from: None` with
    /// `from_now: true` for "skip backlog, just go live," and pair it
    /// with `from_now: false` for the legacy full-backlog behavior.
    /// Existing callers that omit `from_now` keep the prior shape (no
    /// behavior change on the wire for pre-card-7d5b6a65 clients).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<IpcCursor>,
    /// **Card 7d5b6a65.** When `true`, the daemon overrides any `from`
    /// value with the current head cursor at attach time — the client
    /// gets no backlog at all, only events published strictly after
    /// the attach call returns. This is the agent-Monitor live-tail
    /// shape (the existing `from: None` behaviour replayed days of
    /// transcript and fired one notification per historical event).
    ///
    /// Default `false` preserves the prior `from: None` = full-replay
    /// behaviour for tools that depend on it (audit, replay,
    /// codex-hook poll's first-attach catch-up).
    #[serde(default, skip_serializing_if = "is_false")]
    pub from_now: bool,
    /// **Card 7d5b6a65.** When `true`, the daemon emits ONE
    /// [`Response::AttachCursorAdvanced`] summary frame at the end of
    /// the backlog catch-up phase instead of streaming each historical
    /// event individually, then transitions to live tail. Live events
    /// still arrive one-at-a-time as before.
    ///
    /// Has no effect when there is no backlog to coalesce
    /// (`from_now: true` or `from` already at head). Has no effect on
    /// live events — only on the catch-up phase.
    ///
    /// Default `false` preserves the prior event-by-event replay so
    /// audit/replay tools that need to see every historical envelope
    /// keep working unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub coalesce_backlog: bool,
    /// If set, only these kinds are delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<IpcKind>>,
    /// If set, only these delivery classes are delivered (e.g. just
    /// `StreamChunk` for a media tap, or just `Durable` for a transcript
    /// tail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Vec<IpcDelivery>>,
    /// Header predicate evaluated router-side. `Any` (default) matches
    /// all; consumers scope by their `forge.*` projection headers.
    #[serde(default)]
    pub headers: HeaderFilter,
}

#[inline]
fn is_false(value: &bool) -> bool {
    !*value
}

/// Parameters for `AddPeer`. `pubkey_b64` is the URL-safe-no-padding
/// base64 of the 32-byte Ed25519 pubkey (matches the `peer add <spec>`
/// argument shape on the CLI).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddPeerRequest {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
}

/// Parameters for `RemovePeer`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemovePeerRequest {
    pub peer_id: PeerId,
}

/// Parameters for `Send`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendRequest {
    /// Channel UUID. Stable across peers in the same room.
    pub channel: Uuid,
    /// Originating participant identity — the agent/tab that authored
    /// this, established once by the attached client. The daemon is a
    /// broker, not the author: it stamps the envelope `from` with this
    /// so attribution survives (Continuum avatars, chat author,
    /// self-echo). NOT the machine account peer.
    pub from_peer: Uuid,
    /// Stable per-session client id distinguishing tabs that share one
    /// `from_peer`.
    pub from_client: Uuid,
    /// Body text.
    pub text: String,
    /// Optional envelope headers supplied by the caller. Used for
    /// runtime consumer metadata such as `airc.client`.
    #[serde(default)]
    pub headers: Headers,
}

/// Parameters for `Publish`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublishRequest {
    /// Channel UUID. Stable across peers in the same room.
    pub channel: Uuid,
    /// Originating participant identity — the agent/tab that authored
    /// this. The daemon stamps the envelope `from` with it so per-agent
    /// attribution survives the broker (see `SendRequest::from_peer`).
    pub from_peer: Uuid,
    /// Stable per-session client id distinguishing tabs that share one
    /// `from_peer`.
    pub from_client: Uuid,
    /// Envelope kind — full vocabulary so RPC/grid consumers can publish
    /// `Command`/`CommandResult`/`Signal`/`StreamChunk`, not just chat.
    pub kind: IpcKind,
    /// Delivery + retention class. Defaults to `Durable` (chat). Set
    /// `StreamChunk`/`EphemeralLatest`/… for media, game-state, presence
    /// so they route live and never hit the ORM.
    #[serde(default)]
    pub delivery: IpcDelivery,
    /// Addressing. Defaults to `All` (broadcast). `Peer`/`Reply` for
    /// unicast + RPC replies; `Capability` for grid scatter-gather.
    #[serde(default)]
    pub target: IpcTarget,
    /// Correlation id pairing a request to its reply (RPC: Hermes agent
    /// command ↔ tool result; grid capability query ↔ inference result).
    /// `None` for fire-and-forget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
    /// Coalescing key for `EphemeralLatest` — latest-wins is computed per
    /// `(channel, coalesce_key)`. Continuum keys avatar-state/presence by
    /// persona (`"avatar:<persona>"`, `"presence:<peer>"`) so 30 fps of
    /// pose updates collapse to one current value, never persisted.
    /// Ignored for non-`EphemeralLatest` classes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_key: Option<String>,
    /// **Opaque** payload bytes — the daemon routes them, never parses
    /// them. The consumer owns the codec: airc chat encodes a small JSON
    /// `Body`; a WebRTC/game/inference consumer passes raw bytes with
    /// zero serialization. Rides the IPC frame as a CBOR byte-string and
    /// the airc-wire envelope as a raw byte vector — no per-element
    /// serialization on the hot path.
    pub payload: Vec<u8>,
    /// Optional envelope headers supplied by the caller.
    #[serde(default)]
    pub headers: Headers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_serializes_compactly() {
        let encoded = serde_json::to_string(&Request::Ping).unwrap();
        assert_eq!(encoded, r#"{"op":"ping"}"#);
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, Request::Ping);
    }

    #[test]
    fn remove_peer_roundtrips_with_peer_id() {
        let peer_id = PeerId::from_u128(0xabc);
        let encoded =
            serde_json::to_string(&Request::RemovePeer(RemovePeerRequest { peer_id })).unwrap();
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, Request::RemovePeer(RemovePeerRequest { peer_id }));
    }

    #[test]
    fn send_roundtrips_with_typed_fields() {
        let original = Request::Send(SendRequest {
            channel: Uuid::nil(),
            from_peer: Uuid::from_u128(0x1),
            from_client: Uuid::from_u128(0x2),
            text: "hello".to_string(),
            headers: Headers::new(),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn publish_roundtrips_with_typed_body_and_kind() {
        let original = Request::Publish(PublishRequest {
            channel: Uuid::nil(),
            from_peer: Uuid::from_u128(0x1),
            from_client: Uuid::from_u128(0x2),
            kind: IpcKind::Command,
            delivery: IpcDelivery::RequestResponse,
            target: IpcTarget::Capability("inference:gpu".to_string()),
            correlation_id: Some(Uuid::from_u128(0xc0ffee)),
            coalesce_key: None,
            payload: vec![0xde, 0xad, 0xbe, 0xef],
            headers: Headers::new(),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn publish_delivery_defaults_to_durable_when_absent() {
        // A client that omits `delivery` (simple chat publish) must
        // decode as Durable — the streaming classes are opt-in.
        let decoded: Request = serde_json::from_str(
            r#"{"op":"publish","channel":"00000000-0000-0000-0000-000000000000","from_peer":"00000000-0000-0000-0000-000000000001","from_client":"00000000-0000-0000-0000-000000000002","kind":"message","payload":[104,105]}"#,
        )
        .unwrap();
        match decoded {
            Request::Publish(p) => {
                assert_eq!(p.delivery, IpcDelivery::Durable);
                assert_eq!(p.payload, b"hi");
            }
            other => panic!("expected publish, got {other:?}"),
        }
    }

    #[test]
    fn inbox_roundtrips_with_epoch_counter_cursor() {
        let original = Request::Inbox(InboxRequest {
            since: Some(IpcCursor {
                epoch: 3,
                counter: 17,
                event_id: airc_core::EventId::from_u128(0xdead_beef),
            }),
            channel: Some(airc_core::RoomId::from_u128(0x42)),
            limit: Some(64),
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
