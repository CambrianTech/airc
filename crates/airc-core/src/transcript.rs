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
use crate::ids::{ClientId, EventId, PeerId, RoomId};

/// The category of a transcript event. Different kinds may carry
/// different optional fields on `TranscriptEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    /// Conversational message body — the bulk of chat traffic.
    Message,
    /// File / media attachment — `attachment` field populated.
    Attachment,
    /// Delivery / read / applied receipt — `receipt` field populated.
    Receipt,
    /// Presence transition (join, leave, away).
    Presence,
    /// Session-level control envelope (NICK, IDENTIFY, etc.).
    SessionControl,
    /// Substrate-emitted system message (host eviction, error, etc.).
    System,
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
