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
    /// **Card 4b6a0ffa (#33).** The dialable endpoints this daemon
    /// currently advertises (its registry glue's LAN listener, relay,
    /// …). Returned via `Response::RouteEndpoints`. Lets a short-lived
    /// CLI publisher (`airc registry sync`) publish the DAEMON's live
    /// endpoints instead of an endpoint-less beacon — a CLI binding
    /// its own listener would advertise a port that dies with the
    /// process.
    RouteEndpoints,
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
    ScanInbox(ScanInboxRequest),
    /// How many durable events a `ScanInbox` over the same range would match,
    /// with their oldest and newest times, counted by the store: no envelope
    /// crosses the socket.
    CountInbox(CountInboxRequest),
    /// **Card a1562dbc.** The O(1) tip probe: cursor of the newest
    /// durable event on a channel, answered from the router's hot ring
    /// / the sink's index — never by replaying the room. The cheap
    /// "what is the newest cursor?" op for reconnect watermarking, idle
    /// detection, and projection-cache validation; a first-class typed
    /// op, not a `limit: 1` flag on the `Inbox` scan. Returns
    /// `Response::RoomTip`.
    RoomTip(RoomTipRequest),
    /// Resolve one peer's durable identity card from the daemon's
    /// owner-core identity index (`scoped_state`, `user:<peer>`, key
    /// `identity.card`). The attached-client read path for peer names:
    /// a client's LOCAL store never holds foreign peers' cards (they are
    /// observed off the wire into the DAEMON's index), so `peer_alias` /
    /// `peer_identity_card` / `room_roster` resolve names by asking the
    /// daemon — the identity analog of `RoomTip` for the transcript. Returns
    /// `Response::PeerIdentityCard`.
    PeerIdentityCard(PeerIdentityCardRequest),
    /// **airc#1341.** Current PRESENCE on a channel: the router's live
    /// `EphemeralLatest` entries (one per `coalesce_key`), answered from the
    /// in-memory ephemeral cache — never by paging durable history. Presence is
    /// state, not an event. Returns `Response::Presence`.
    Presence(PresenceRequest),
    /// **#270/#241.** The scope's durable subscribed-room registry
    /// (name + room id + joined-at + default flag, parted rooms
    /// excluded). The membership read that lets an attached client
    /// (continuum's nav, a TUI room list) show EVERY room the account
    /// is in — not just the rooms that happened to emit traffic since
    /// boot. Returned via `Response::Rooms`.
    ListRooms,
    /// **#1306 slice 2.** Per-peer end-to-end delivery accounting from
    /// the daemon's delivery ledger — attempts, acks, last confirmed
    /// delivery, measured RTT, suspect verdict. THE delivery-truth read:
    /// doctor reports "last confirmed delivery to X: N ago" from this
    /// instead of inferring health from TCP connection state. Returned
    /// via `Response::DeliveryStats`. Empty until the routed forwarder
    /// has attempted at least one cross-machine delivery.
    DeliveryStats,
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
    /// **Continuum #297.** When set, the daemon filters to these kinds
    /// BEFORE applying `limit` — the page is the newest N events *of
    /// these kinds*, not the survivors of a raw newest-N. Post-filtering
    /// client-side cannot express that: three personas streaming ~4
    /// `StreamChunk` frames/sec evict every durable `Message` from a
    /// 50-event page, deafening working personas to direction.
    ///
    /// `None` (and, on the daemon side, an empty vec — filtering to
    /// nothing is never what a caller means) is exactly today's
    /// unfiltered behavior. Wire-compat both directions: old daemons
    /// ignore the field; old clients omit it (`skip_serializing_if`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<IpcKind>>,
    /// History, BACKWARD: the newest `limit` events strictly before this
    /// cursor, oldest first. How a consumer reads past the one page an
    /// attach streams (Joel, 2026-10-06: attach anchors at now and pages
    /// back on demand). Exclusive with `since`. Wire-compatible both ways.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<IpcCursor>,
}

/// Parameters for `RoomTip` (card a1562dbc). The channel is mandatory:
/// the tip is a per-room property in the owner-core model, exactly like
/// `Inbox` paging.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomTipRequest {
    /// The channel (room) whose durable tip is being probed.
    pub channel: airc_core::RoomId,
}

/// Parameters for `Presence` (airc#1341). The channel is mandatory: presence
/// is a per-room property, like the tip.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresenceRequest {
    /// The channel (room) whose live presence is being read.
    pub channel: airc_core::RoomId,
}

/// Parameters for `PeerIdentityCard`. The peer whose durable
/// identity-index row (`user:<peer>` / `identity.card`) is resolved.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerIdentityCardRequest {
    /// The peer whose published identity card is being resolved.
    pub peer_id: PeerId,
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
    /// A bookmark: the stream opens with ONE page of the newest events
    /// after this cursor (at most the daemon's page, 10), then a
    /// `AttachCursorAdvanced` summary when it left older unread out, then
    /// live. The gap is never replayed (Joel, 2026-10-06: "it should be
    /// impossible"); older history is paged backward with
    /// `Inbox { before }`.
    After(IpcCursor),
    /// The same one page, of the room's newest events, then the summary,
    /// then live. Never the full transcript: page back with
    /// `Inbox { before }` for history.
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
    /// **Card 7d5b6a65, now implied.** The daemon serves every start in
    /// the past as one page plus a summary whether or not this is set; it
    /// is kept on the wire so older clients' requests still decode.
    #[serde(default, skip_serializing_if = "is_false")]
    coalesce_backlog: bool,
    /// **Now ignored.** The page an attach streams is the daemon's own
    /// (10 events), not the client's ask: a request can no longer widen
    /// it. Kept on the wire so older clients' requests still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backlog_tail: Option<u32>,
    /// When `true`, the daemon emits a throttled (1 Hz) live
    /// [`Response::AttachCursorAdvanced`] after forwarded events so a
    /// consumer that PERSISTS its cursor can advance its watermark
    /// (continuum #261). Off by default: a consumer that does not
    /// persist cursors has nothing to do with the frame, and every
    /// in-tree consumer today reads only the seam summary — the
    /// unconditional heartbeat was 3,100 warn lines/min on a 16-citizen
    /// core (airc #1411/#1416). Wire-compat: omitted when unset.
    #[serde(default, skip_serializing_if = "is_false")]
    cursor_heartbeat: bool,
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
    /// A CHANNEL SET served on this one stream, each room with its own
    /// resume point. The daemon fans the set out router-side and routes
    /// every `Event` frame by the envelope's own `channel`, so one
    /// subscriber holds ONE socket for all its rooms instead of one per
    /// room (measured 2026-10-04: a 15-subscriber core held ~960 attach
    /// sockets to its daemon). Wire-compat: omitted when unset, so an
    /// older daemon ignores it and rejects the channel-less request as
    /// it always did; a client picks this shape only when the daemon's
    /// status reports `attach_channel_sets`. When set, `channel` /
    /// `from` / `from_now` are not consulted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channels: Option<Vec<ChannelAttach>>,
}

/// One room of a channel-set attach: the room and where its stream
/// starts. `from: None` is the live edge; `Some` resumes strictly after
/// that cursor, replaying the durable gap exactly as a single-channel
/// [`AttachStart::After`] does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelAttach {
    pub channel: airc_core::RoomId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<IpcCursor>,
}

impl ChannelAttach {
    /// The typed start this entry encodes. A set entry has no
    /// `from_now` flag: no cursor IS the live edge, never the transcript
    /// start — a set exists for live subscribers that track their own
    /// cursors, and a full-history replay of N rooms on one socket is
    /// not a shape anyone asked for.
    pub fn start(&self) -> AttachStart {
        match self.from {
            Some(cursor) => AttachStart::After(cursor),
            None => AttachStart::Live,
        }
    }
}

impl AttachRequest {
    /// Attach to a CHANNEL SET on one stream. Each entry carries its
    /// own start; filters apply to every room in the set. An empty set
    /// is rejected by the daemon. See [`Self::channels`] for the
    /// version contract.
    pub fn channel_set(channels: Vec<ChannelAttach>) -> Self {
        Self {
            channel: None,
            from: None,
            from_now: false,
            coalesce_backlog: false,
            backlog_tail: None,
            cursor_heartbeat: false,
            kinds: None,
            delivery: None,
            headers: HeaderFilter::default(),
            channels: Some(channels),
        }
    }

    /// The channel set this request attaches to, if it is a set attach.
    pub fn channels(&self) -> Option<&[ChannelAttach]> {
        self.channels.as_deref()
    }

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
            backlog_tail: None,
            cursor_heartbeat: false,
            kinds: None,
            delivery: None,
            headers: HeaderFilter::default(),
            channels: None,
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

    /// How many most-recent backlog events are streamed at the seam
    /// (the rest are coalesced). `None` = coalesce everything.
    pub fn backlog_tail(&self) -> Option<u32> {
        self.backlog_tail
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

    /// Implied since every past start is served as one page plus a
    /// summary; kept so existing callers and older wire shapes still work.
    pub fn with_coalesced_backlog(mut self) -> Self {
        self.coalesce_backlog = true;
        self
    }

    /// Ignored by the daemon: the page an attach streams is the daemon's
    /// own (10 events). Kept so older wire shapes still decode.
    pub fn with_backlog_tail(mut self, n: u32) -> Self {
        self.backlog_tail = Some(n);
        self
    }

    /// Ask for the live cursor heartbeat: one
    /// [`Response::AttachCursorAdvanced`] per second after forwarded
    /// events. Only a consumer that persists its cursor should set this.
    pub fn with_cursor_heartbeat(mut self) -> Self {
        self.cursor_heartbeat = true;
        self
    }

    /// Whether this attach asked for the live cursor heartbeat.
    pub fn wants_cursor_heartbeat(&self) -> bool {
        self.cursor_heartbeat
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
            backlog_tail: self.backlog_tail,
            cursor_heartbeat: self.cursor_heartbeat,
            kinds: self.kinds,
            delivery: self.delivery,
            headers: self.headers,
            channels: self.channels,
        }
    }
}

/// Owned view of an [`AttachRequest`] for the serving side. Produced by
/// [`AttachRequest::into_parts`]; not constructible into a request.
pub struct AttachParts {
    pub channel: Option<airc_core::RoomId>,
    pub start: AttachStart,
    pub coalesce_backlog: bool,
    pub backlog_tail: Option<u32>,
    /// See [`AttachRequest::with_cursor_heartbeat`].
    pub cursor_heartbeat: bool,
    pub kinds: Option<Vec<IpcKind>>,
    pub delivery: Option<Vec<IpcDelivery>>,
    pub headers: HeaderFilter,
    /// See [`AttachRequest::channel_set`]. `Some` makes this a set
    /// attach; `channel` / `start` are then not consulted.
    pub channels: Option<Vec<ChannelAttach>>,
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
mod tests;

/// Predicate-scoped forward scan (or initial tail), bounded by a fixed snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanInboxRequest {
    pub channel: airc_core::RoomId,
    pub after: IpcCursor,
    pub through: IpcCursor,
    pub headers: HeaderFilter,
    /// Only events that could be addressed to this peer (its typed target, or
    /// a body opening with an `@name`), selected by the store before `limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addressed_to: Option<airc_core::AddresseeFilter>,
    pub limit: usize,
    pub tail: bool,
}

/// Count the events a [`ScanInboxRequest`] over `(after, through]` would match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CountInboxRequest {
    pub channel: airc_core::RoomId,
    pub after: IpcCursor,
    pub through: IpcCursor,
    pub headers: HeaderFilter,
    /// The asking runtime's own sends, left out of the count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_self: Option<airc_core::RuntimeSelf>,
}
