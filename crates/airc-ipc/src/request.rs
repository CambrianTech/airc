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
    /// **Card a1562dbc.** The O(1) tip probe: cursor of the newest
    /// durable event on a channel, answered from the router's hot ring
    /// / the sink's index — never by replaying the room. The cheap
    /// "what is the newest cursor?" op for reconnect watermarking, idle
    /// detection, and projection-cache validation; a first-class typed
    /// op, not a `limit: 1` flag on the `Inbox` scan. Returns
    /// `Response::RoomTip`.
    RoomTip(RoomTipRequest),
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

/// Parameters for `RoomTip` (card a1562dbc). The channel is mandatory:
/// the tip is a per-room property in the owner-core model, exactly like
/// `Inbox` paging.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomTipRequest {
    /// The channel (room) whose durable tip is being probed.
    pub channel: airc_core::RoomId,
}

/// Where an attach stream starts. One intentional choice — not a
/// `from`/`from_now` flag pair whose precedence lived in a server-side
/// comment (card c0cb6cdc; the pair let `..Default::default()` mean
/// "replay days of transcript," which is how bf0b5790 happened).
///
/// `Live` is the default because it is the only start that can never
/// flood a consumer. Both replaying variants must be named at the call
/// site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttachStart {
    /// Start at the channel's live edge: deliver only events published
    /// after the subscription is registered. The subscribe contract.
    #[default]
    Live,
    /// Resume strictly after this cursor — replay the gap the client
    /// missed while detached, then continue live with no duplicate at
    /// the seam.
    After(IpcCursor),
    /// Replay the full transcript before going live. Audit/replay
    /// tools only — on a long-lived room this is days of backlog, so
    /// pair it with [`AttachRequest::with_coalesced_backlog`] unless
    /// every historical envelope is genuinely wanted.
    FromTranscriptStart,
}

/// Parameters for `Attach`. Filters are applied **router-side** so the
/// daemon never fans out events the consumer would discard — the
/// adaptable/performant subscription: Hermes attaches to
/// Command/CommandResult, Continuum scopes by `forge.continuum.*`
/// headers, a game attaches to one room's StreamChunk only.
///
/// Fields are private: construction goes through [`AttachRequest::new`]
/// / [`AttachRequest::live`] so the start position is always a typed
/// [`AttachStart`], never an implicit flag combination. The wire shape
/// is unchanged (`from` + `from_now` encode the enum), so version-skewed
/// daemon/client pairs interoperate: an old client's bare request decodes
/// as [`AttachStart::FromTranscriptStart`], exactly its legacy meaning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachRequest {
    /// The channel (room) to attach to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<airc_core::RoomId>,
    /// Wire encoding of [`AttachStart::After`]. See [`Self::start`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<IpcCursor>,
    /// Wire encoding of [`AttachStart::Live`] (card 7d5b6a65): the
    /// daemon starts at the channel head, ignoring `from`. See
    /// [`Self::start`] — the precedence lives there, nowhere else.
    #[serde(default, skip_serializing_if = "is_false")]
    from_now: bool,
    /// **Card 7d5b6a65.** When `true`, the daemon emits ONE
    /// [`Response::AttachCursorAdvanced`] summary frame at the end of
    /// the backlog catch-up phase instead of streaming each historical
    /// event individually, then transitions to live tail. Orthogonal to
    /// [`AttachStart`]: it shapes how backlog is delivered, not where
    /// the stream starts, and is a no-op when there is no backlog
    /// (`Live`, or a cursor already at head).
    #[serde(default, skip_serializing_if = "is_false")]
    coalesce_backlog: bool,
    /// If set, only these kinds are delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kinds: Option<Vec<IpcKind>>,
    /// If set, only these delivery classes are delivered (e.g. just
    /// `StreamChunk` for a media tap, or just `Durable` for a transcript
    /// tail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery: Option<Vec<IpcDelivery>>,
    /// Header predicate evaluated router-side. `Any` (default) matches
    /// all; consumers scope by their `forge.*` projection headers.
    #[serde(default)]
    headers: HeaderFilter,
}

impl AttachRequest {
    /// Attach to `channel`, starting at `start`. No filters: every
    /// event class on the channel is delivered. Narrow with the
    /// `with_*` builders.
    pub fn new(channel: airc_core::RoomId, start: AttachStart) -> Self {
        let (from, from_now) = match start {
            AttachStart::Live => (None, true),
            AttachStart::After(cursor) => (Some(cursor), false),
            AttachStart::FromTranscriptStart => (None, false),
        };
        Self {
            channel: Some(channel),
            from,
            from_now,
            coalesce_backlog: false,
            kinds: None,
            delivery: None,
            headers: HeaderFilter::default(),
        }
    }

    /// [`Self::new`] with [`AttachStart::Live`] — the common shape.
    pub fn live(channel: airc_core::RoomId) -> Self {
        Self::new(channel, AttachStart::Live)
    }

    /// Decode the wire flag pair back into the typed start position.
    /// This is the ONLY place the `from_now`-overrides-`from`
    /// precedence exists.
    pub fn start(&self) -> AttachStart {
        if self.from_now {
            AttachStart::Live
        } else if let Some(cursor) = self.from {
            AttachStart::After(cursor)
        } else {
            AttachStart::FromTranscriptStart
        }
    }

    /// The channel this request attaches to, if one was set (requests
    /// from legacy clients may omit it; the daemon rejects those).
    pub fn channel(&self) -> Option<airc_core::RoomId> {
        self.channel
    }

    /// Whether backlog catch-up is collapsed into one summary frame.
    pub fn coalesces_backlog(&self) -> bool {
        self.coalesce_backlog
    }

    /// Deliver only these event kinds.
    pub fn with_kinds(mut self, kinds: Vec<IpcKind>) -> Self {
        self.kinds = Some(kinds);
        self
    }

    /// Deliver only these delivery classes.
    pub fn with_delivery(mut self, delivery: Vec<IpcDelivery>) -> Self {
        self.delivery = Some(delivery);
        self
    }

    /// Scope by header predicate (router-side).
    pub fn with_headers(mut self, headers: HeaderFilter) -> Self {
        self.headers = headers;
        self
    }

    /// Collapse backlog catch-up into one summary frame (card 7d5b6a65).
    pub fn with_coalesced_backlog(mut self) -> Self {
        self.coalesce_backlog = true;
        self
    }

    /// Destructure for the daemon's attach handler: moves the filter
    /// vectors out (no clone) with the start already decoded. One-way —
    /// there is no path from parts back to a request, so the typed
    /// start cannot be bypassed.
    pub fn into_parts(self) -> AttachParts {
        let start = self.start();
        AttachParts {
            channel: self.channel,
            start,
            coalesce_backlog: self.coalesce_backlog,
            kinds: self.kinds,
            delivery: self.delivery,
            headers: self.headers,
        }
    }
}

/// Owned view of an [`AttachRequest`] for the serving side. Produced by
/// [`AttachRequest::into_parts`]; not constructible into a request.
pub struct AttachParts {
    pub channel: Option<airc_core::RoomId>,
    pub start: AttachStart,
    pub coalesce_backlog: bool,
    pub kinds: Option<Vec<IpcKind>>,
    pub delivery: Option<Vec<IpcDelivery>>,
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

    fn cursor(counter: u64) -> IpcCursor {
        IpcCursor {
            epoch: 1,
            counter,
            event_id: airc_core::EventId::from_u128(0xfeed),
        }
    }

    /// Card c0cb6cdc: every `AttachStart` survives the wire round-trip —
    /// the legacy flag-pair encoding decodes back to the same typed start.
    #[test]
    fn attach_start_roundtrips_through_wire_encoding() {
        for start in [
            AttachStart::Live,
            AttachStart::After(cursor(7)),
            AttachStart::FromTranscriptStart,
        ] {
            let request = AttachRequest::new(airc_core::RoomId(Uuid::nil()), start);
            let encoded = serde_json::to_string(&request).unwrap();
            let decoded: AttachRequest = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded.start(), start, "round-trip of {start:?}");
            assert_eq!(decoded, request);
        }
    }

    /// Card 13cae1db: the EXACT serialized bytes for each `AttachStart`
    /// variant ARE the cross-version contract. The decode side is pinned
    /// by the raw-JSON tests below; this pins the encode side, so a
    /// SYMMETRIC serde rename (e.g. `from_now` → `start_now` on both the
    /// field and these literals) still fails a test instead of silently
    /// breaking old daemons that only speak the legacy flag pair.
    #[test]
    fn attach_start_wire_bytes_are_pinned_per_variant() {
        let channel = airc_core::RoomId(Uuid::nil());
        for (start, expected) in [
            (
                AttachStart::Live,
                r#"{"channel":"00000000-0000-0000-0000-000000000000","from_now":true,"headers":"any"}"#,
            ),
            (
                AttachStart::After(cursor(7)),
                r#"{"channel":"00000000-0000-0000-0000-000000000000","from":{"epoch":1,"counter":7,"event_id":"00000000-0000-0000-0000-00000000feed"},"headers":"any"}"#,
            ),
            (
                AttachStart::FromTranscriptStart,
                r#"{"channel":"00000000-0000-0000-0000-000000000000","headers":"any"}"#,
            ),
        ] {
            let encoded = serde_json::to_string(&AttachRequest::new(channel, start)).unwrap();
            assert_eq!(encoded, expected, "wire bytes of {start:?}");
        }
    }

    /// Card c0cb6cdc: a bare request from a pre-AttachStart client
    /// (no `from`, no `from_now` on the wire) keeps its legacy meaning —
    /// full transcript replay — so version-skewed peers interoperate.
    #[test]
    fn legacy_bare_attach_decodes_as_transcript_start() {
        let legacy = format!(r#"{{"channel":"{}"}}"#, Uuid::nil());
        let decoded: AttachRequest = serde_json::from_str(&legacy).unwrap();
        assert_eq!(decoded.start(), AttachStart::FromTranscriptStart);
        assert!(!decoded.coalesces_backlog());
    }

    /// Card c0cb6cdc: `from_now` wins over a cursor on the wire — the
    /// precedence exists in exactly one place (`AttachRequest::start`),
    /// pinned here so it never silently drifts.
    #[test]
    fn from_now_takes_precedence_over_cursor_on_the_wire() {
        let skewed = format!(
            r#"{{"channel":"{}","from":{},"from_now":true}}"#,
            Uuid::nil(),
            serde_json::to_string(&cursor(3)).unwrap(),
        );
        let decoded: AttachRequest = serde_json::from_str(&skewed).unwrap();
        assert_eq!(decoded.start(), AttachStart::Live);
    }

    /// Card c0cb6cdc: the dangerous start must be named — the safe
    /// default of the typed enum is `Live`.
    #[test]
    fn attach_start_default_is_live() {
        assert_eq!(AttachStart::default(), AttachStart::Live);
    }

    #[test]
    fn into_parts_moves_filters_with_decoded_start() {
        let parts = AttachRequest::new(
            airc_core::RoomId(Uuid::nil()),
            AttachStart::After(cursor(9)),
        )
        .with_kinds(vec![IpcKind::Command])
        .with_coalesced_backlog()
        .into_parts();
        assert_eq!(parts.start, AttachStart::After(cursor(9)));
        assert!(parts.coalesce_backlog);
        assert_eq!(parts.kinds.as_deref(), Some(&[IpcKind::Command][..]));
    }

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

    /// Card a1562dbc: the EXACT wire bytes of `RoomTip` are the
    /// cross-version contract — `op` tag and `channel` field name
    /// pinned as literal strings. A serde rename (even a symmetric
    /// one) must fail here, not silently strand old daemons.
    #[test]
    fn room_tip_wire_bytes_are_pinned() {
        let request = Request::RoomTip(RoomTipRequest {
            channel: airc_core::RoomId(Uuid::nil()),
        });
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(
            encoded,
            r#"{"op":"room_tip","channel":"00000000-0000-0000-0000-000000000000"}"#
        );
    }

    /// Card a1562dbc: decode-side pin — the literal wire JSON an
    /// already-shipped client would send must decode to the typed
    /// variant. Pins the decode half independently of the encode half
    /// so a symmetric rename cannot pass both.
    #[test]
    fn room_tip_literal_wire_json_decodes() {
        let decoded: Request = serde_json::from_str(
            r#"{"op":"room_tip","channel":"00000000-0000-0000-0000-000000000042"}"#,
        )
        .unwrap();
        assert_eq!(
            decoded,
            Request::RoomTip(RoomTipRequest {
                channel: airc_core::RoomId::from_u128(0x42),
            })
        );
    }

    /// Card a1562dbc: adding the `RoomTip` variant must not perturb the
    /// decoding of pre-existing ops — the literal pre-RoomTip wire
    /// bytes of a neighbouring op still decode unchanged.
    #[test]
    fn room_tip_addition_keeps_existing_inbox_wire_compat() {
        let decoded: Request = serde_json::from_str(
            r#"{"op":"inbox","channel":"00000000-0000-0000-0000-000000000042","limit":1}"#,
        )
        .unwrap();
        assert_eq!(
            decoded,
            Request::Inbox(InboxRequest {
                since: None,
                channel: Some(airc_core::RoomId::from_u128(0x42)),
                limit: Some(1),
            })
        );
    }

    #[test]
    fn stop_serializes_compactly() {
        assert_eq!(
            serde_json::to_string(&Request::Stop).unwrap(),
            r#"{"op":"stop"}"#
        );
    }
}
