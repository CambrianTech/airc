//! Transcript event — the durable record of one room interaction.
//!
//! A `TranscriptEvent` is the canonical "something happened in this room"
//! record. Different `TranscriptKind`s carry different optional payloads
//! (an Attachment-kind event has the `attachment` field populated; a
//! Receipt-kind event has the `receipt` field populated).
//!
//! The lamport + event_id pair is what cursoring + ordering ride on (see
//! `cursor` module).

use serde::{Deserialize, Serialize};

use crate::body::Body;
use crate::cursor::TranscriptCursor;
use crate::filter::SelfFilter;
use crate::headers::Headers;
use crate::ids::{ClientId, EventId, PeerId, RoomId};

/// The category of a transcript event. Different kinds may carry
/// different optional fields on `TranscriptEvent`.
///
/// The first six kinds are chat-shaped and consumer-authored:
/// every send is one of these. The remaining "lifecycle" kinds are
/// **substrate-authored** — emitted by airc itself when state
/// transitions happen (room joined, peer arrived, wire established,
/// etc.). They share the same envelope shape so consumers
/// subscribe with the same `EventFilter` surface, but the substrate
/// signs them from its own identity, and the `body` carries a
/// structured JSON payload documented per-variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    /// Conversational message body — the bulk of chat traffic.
    Message,
    /// File / media attachment — `attachment` field populated.
    Attachment,
    /// Delivery / read / applied receipt — `receipt` field populated.
    Receipt,
    /// Presence transition (join, leave, away) — IRC analog.
    /// Distinct from the lifecycle kinds below: `Presence` is a
    /// consumer-authored chat-shaped event ("I am away"), while
    /// `PeerArrived`/`PeerDeparted` are substrate-authored
    /// transitions of the local peer registry.
    Presence,
    /// Session-level control envelope (NICK, IDENTIFY, etc.).
    SessionControl,
    /// Substrate-emitted system message (host eviction, error, etc.).
    System,

    // --- Lifecycle kinds (Phase 2 of GRID-SUBSTRATE-AUDIT) ---
    // Substrate-authored events consumers subscribe to instead of
    // polling. Each carries a JSON body documented in
    // `airc-lib::lifecycle`.
    /// A new peer was enrolled in the local peer-trust registry.
    /// Body: `{ "peer_id": <uuid>, "via": "<source>" }` where
    /// `via` is `"invite"` / `"account_registry"` / `"manual"`.
    PeerArrived,
    /// A peer was removed from the local trust registry (kick,
    /// teardown). Body: `{ "peer_id": <uuid>, "reason": "<text>" }`.
    PeerDeparted,
    /// A wire (channel transport) became available to the local
    /// scope — the wire subscriber attached successfully.
    /// Body: `{ "wire": "<path>", "channel_name": "<name>" }`.
    WireEstablished,
    /// A wire subscriber failed or was torn down. Body:
    /// `{ "wire": "<path>", "reason": "<text>" }`.
    WireLost,
    /// This scope joined a room. Body: `{ "channel_name": "<name>",
    /// "room_id": <uuid>, "wire": "<path>", "is_default": bool }`.
    RoomJoined,
    /// This scope parted a room. Body: `{ "channel_name": "<name>",
    /// "room_id": <uuid> }`.
    RoomParted,
    /// A runtime consumer's cursor advanced. Body:
    /// `{ "consumer_id": "<id>", "lamport": <u64>,
    ///    "event_id": <uuid> }`. Useful for "I've caught up to
    /// here" signals between cooperating consumers.
    SubscriptionAdvanced,
}

impl TranscriptKind {
    /// True for substrate-authored lifecycle kinds (the Phase 2
    /// additions). Consumers that want only chat-shaped events
    /// filter these out via `EventFilter::kinds`.
    pub fn is_lifecycle(self) -> bool {
        matches!(
            self,
            TranscriptKind::PeerArrived
                | TranscriptKind::PeerDeparted
                | TranscriptKind::WireEstablished
                | TranscriptKind::WireLost
                | TranscriptKind::RoomJoined
                | TranscriptKind::RoomParted
                | TranscriptKind::SubscriptionAdvanced
        )
    }
}

/// Who a transcript event is addressed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionTarget {
    /// Broadcast to everyone in the room.
    All,
    /// Direct address to one peer (DM-style).
    Peer(PeerId),
    /// Addressed to a sibling room (cross-room reference).
    Room(RoomId),
}

/// One durable record of "something happened in this room."
///
/// Constructed at the receive side from a wire envelope and persisted by
/// the store layer. Consumers read these via cursor-paged queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptEvent {
    pub event_id: EventId,
    pub room_id: RoomId,
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub kind: TranscriptKind,
    pub occurred_at_ms: u64,
    pub lamport: u64,
    pub target: MentionTarget,
    /// Small envelope metadata for routing/filtering without parsing body.
    #[serde(default)]
    pub headers: Headers,
    /// Opaque payload — consumer-defined JSON or binary. See [`Body`].
    /// Replaces the legacy `Option<String>` shape; consumers wanting
    /// plain chat text use `Body::text("...")` and recover it via
    /// `body.as_ref().and_then(Body::as_text)`.
    pub body: Option<Body>,
    pub attachment: Option<crate::attachment::AttachmentManifest>,
    pub receipt: Option<crate::receipt::Receipt>,
    pub metadata: serde_json::Value,
}

impl TranscriptEvent {
    /// Extract this event's cursor — the (lamport, event_id) pair that
    /// callers use for "fetch since" and "fetch before" paging.
    pub fn cursor(&self) -> TranscriptCursor {
        TranscriptCursor {
            lamport: self.lamport,
            event_id: self.event_id,
        }
    }

    /// Is this event from the receiver's own peer/client (and should be
    /// filtered from display per the filter mode)?
    pub fn is_self_echo(&self, peer_id: &PeerId, client_id: &ClientId, filter: SelfFilter) -> bool {
        match filter {
            SelfFilter::IncludeAll => false,
            SelfFilter::ExcludeSameClient => &self.client_id == client_id,
            SelfFilter::ExcludeSamePeer => &self.peer_id == peer_id,
        }
    }
}
