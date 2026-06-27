//! The generic envelope (§2 of AIRC-EVENT-SERVER).
//!
//! One primitive carries everything: a chat message, a `data:*` event, a
//! `screenshot` command, a WebRTC signaling frame. They are distinguished by
//! [`Kind`] + [`DeliveryClass`], never by living on different buses. The
//! server routes on `channel` / `target` / `headers` / `delivery` and
//! **never interprets `payload`** — that opacity keeps it generic across
//! towers.
//!
//! Reuses `airc-core` identity/header types (`RoomId`, `PeerId`, `ClientId`,
//! `EventId`, `Headers`) so the envelope sits on the same substrate as the
//! durable `TranscriptEvent`. The durable mapping (`Envelope` ↔
//! `TranscriptEvent`) is a later-slice concern behind `DurableSink`; this
//! crate keeps the hot-path type free of ORM shape.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::{ClientId, EventId, Headers, PeerId, RoomId};

/// Owner-assigned total order = `(epoch, counter)` (§2, §3.8).
///
/// `epoch` is persisted and bumped on every daemon start; `counter` is the
/// in-memory monotonic. Deliver-first (§3.3) can ack a `counter` the ORM has
/// not flushed yet, so a counter rebuilt from ORM-max after a crash would
/// *reissue* numbers live subscribers already observed. Bumping `epoch` makes
/// post-crash events sort strictly after anything pre-crash regardless of
/// counter rewind. A bare `u64` counter is **not** safe here.
///
/// The total order within one owner+channel is `(epoch, counter, event_id)`.
/// `Ord` derives lexicographically over the field order, which is exactly that
/// — `epoch` dominates, then `counter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Seq {
    pub epoch: u64,
    pub counter: u64,
}

impl Seq {
    pub fn new(epoch: u64, counter: u64) -> Self {
        Self { epoch, counter }
    }
}

impl std::fmt::Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.epoch, self.counter)
    }
}

/// Who an envelope is addressed to (§2).
///
/// `Capability` is the 1-to-N capability/query target (§3.9 fan-out /
/// scatter-gather). Slice 1 carries the variant so the orchestration is not
/// *precluded*; the grid-router that resolves a capability to a peer set lands
/// in a later slice. Within slice 1 it routes like `All` (the resolver is
/// absent), but the shape is here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Broadcast to every subscriber on the channel.
    All,
    /// A named endpoint address (env/grid scope) — resolved by the router's
    /// endpoint table in a later slice.
    Endpoint(String),
    /// A specific peer.
    Peer(PeerId),
    /// The reply leg of a request/response or command/result correlation.
    Reply(Uuid),
    /// A capability/query (e.g. `inference:* on a gpu peer`) the grid-router
    /// resolves to a peer *set* — 1-to-N addressing (§3.9). Slice-1
    /// placeholder so fan-out/scatter-gather is not precluded.
    Capability(String),
}

/// The category of an envelope (§2). Routing/retention keys off this plus
/// [`DeliveryClass`]; the payload is never parsed to discover it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Message,
    Event,
    Command,
    CommandResult,
    Signal,
    StreamChunk,
    /// Out-of-band control — e.g. a cancellation addressed to a
    /// `correlation_id` (§3.9 long-running ops). Slice-1 placeholder.
    Control,
}

/// How an envelope is delivered + retained (§2, §3.3, §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryClass {
    /// Becomes an ORM row via the write-behind path; the durability source of
    /// truth. The only class that reaches [`crate::DurableSink`].
    Durable,
    /// Coalesced latest-wins by `(channel, coalesce_key)` in-memory with TTL
    /// (§3.4). 1000 typing updates → one latest value. **Never** an ORM row.
    EphemeralLatest,
    /// A bounded ephemeral window (recent N), in-memory only. Carried for
    /// completeness; slice-1 treats it as live-fan-out + ring like `Durable`
    /// minus persistence.
    EphemeralWindow,
    /// Request leg of a request/response (§3.9). Routed live; correlated by
    /// `correlation_id`. Not persisted by default.
    RequestResponse,
    /// A chunk of a longer stream (progress, media-control). Routed live; not
    /// persisted by default.
    StreamChunk,
}

impl DeliveryClass {
    /// True iff an envelope of this class must reach the durable tier.
    pub fn is_durable(self) -> bool {
        matches!(self, DeliveryClass::Durable)
    }

    /// True iff an envelope of this class coalesces latest-wins and must
    /// **never** reach the durable tier (the firehose keystone, §3.4).
    pub fn is_ephemeral_latest(self) -> bool {
        matches!(self, DeliveryClass::EphemeralLatest)
    }
}

/// The generic envelope (§2). `payload` is opaque consumer bytes.
///
/// `seq` and `occurred_at_ms` are **owner-stamped** — they are assigned by
/// [`crate::EventRouter::publish`] and are *not* part of the sender's
/// signature scope (§2 signature scope). A freshly-authored envelope leaves
/// `seq` unset until publish; the convenience constructors below build the
/// sender-authored shape and the router fills the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    /// Stable across replay — two subscribers replaying the same event see the
    /// same `event_id`.
    pub event_id: EventId,
    /// The room/stream this envelope belongs to.
    pub channel: RoomId,
    /// Sender identity + session: `(peer, client)`.
    pub from: (PeerId, ClientId),
    /// Addressing (§2).
    pub target: Target,
    pub kind: Kind,
    pub delivery: DeliveryClass,
    /// Owner-assigned order `(epoch, counter)` (§2, §3.8). Assigned at publish.
    pub seq: Seq,
    /// Owner-stamped wall clock via an injectable [`crate::Clock`]
    /// (deterministic tests, §9). Assigned at publish.
    pub occurred_at_ms: u64,
    /// Command ↔ result, request ↔ response correlation.
    pub correlation_id: Option<Uuid>,
    /// Coalescing key for [`DeliveryClass::EphemeralLatest`] (§3.4).
    pub coalesce_key: Option<String>,
    /// Routable metadata; airc routes on these, never parses payload.
    pub headers: Headers,
    /// OPAQUE consumer-typed payload.
    pub payload: Bytes,
}

impl Envelope {
    /// Construct a sender-authored envelope. `seq` and `occurred_at_ms` are
    /// placeholders (`Seq { 0, 0 }` / `0`) — [`crate::EventRouter::publish`]
    /// overwrites them with owner-stamped values. Use the builder setters for
    /// the optional fields.
    pub fn new(
        channel: RoomId,
        from: (PeerId, ClientId),
        kind: Kind,
        delivery: DeliveryClass,
        payload: Bytes,
    ) -> Self {
        Self {
            event_id: EventId::new(),
            channel,
            from,
            target: Target::All,
            kind,
            delivery,
            seq: Seq::new(0, 0),
            occurred_at_ms: 0,
            correlation_id: None,
            coalesce_key: None,
            headers: BTreeMap::new(),
            payload,
        }
    }

    /// Set the addressing target.
    pub fn with_target(mut self, target: Target) -> Self {
        self.target = target;
        self
    }

    /// Set the coalescing key (for [`DeliveryClass::EphemeralLatest`]).
    pub fn with_coalesce_key(mut self, key: impl Into<String>) -> Self {
        self.coalesce_key = Some(key.into());
        self
    }

    /// Set the correlation id.
    pub fn with_correlation_id(mut self, id: Uuid) -> Self {
        self.correlation_id = Some(id);
        self
    }

    /// Set one header.
    pub fn with_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Override the stable `event_id` — used by deterministic tests so a
    /// replayed event is identity-comparable.
    pub fn with_event_id(mut self, id: EventId) -> Self {
        self.event_id = id;
        self
    }

    /// The cursor position of this envelope: `(seq, event_id)` (§3.5).
    pub fn cursor(&self) -> Cursor {
        Cursor {
            seq: self.seq,
            event_id: self.event_id,
        }
    }
}

/// A per-owner-per-channel replay position (§3.5).
///
/// Cursor = `(seq, event_id)`, `seq = (epoch, counter)`. A channel's total
/// order is authoritative only *within one owner daemon* — cross-machine order
/// of a shared channel is deliberately NOT assumed (§9). Slice 1 must not bake
/// in a single global authority, so this type is intentionally per-owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub seq: Seq,
    pub event_id: EventId,
}

impl Cursor {
    pub fn new(seq: Seq, event_id: EventId) -> Self {
        Self { seq, event_id }
    }

    /// True iff `self` is strictly before `other` in total order:
    /// `(epoch, counter)` first, `event_id` as deterministic tiebreaker.
    ///
    /// The tiebreaker only ever matters if two events share an exact `seq`,
    /// which the monotonic counter forbids within one epoch; it is here for
    /// defense-in-depth and to give `Cursor` a total order independent of how
    /// `seq` was produced.
    pub fn is_before(&self, other: &Cursor) -> bool {
        match self.seq.cmp(&other.seq) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => self.event_id.0 < other.event_id.0,
        }
    }

    /// True iff an event with this cursor is strictly *after* `gate` — the
    /// "deliver everything strictly after my cursor" predicate (§3.5).
    pub fn is_after(&self, gate: &Cursor) -> bool {
        gate.is_before(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_orders_epoch_dominant() {
        // §3.8: a post-crash event (higher epoch, possibly lower counter)
        // sorts strictly AFTER a pre-crash event even when the counter rewinds.
        let pre = Seq::new(1, 1000);
        let post = Seq::new(2, 0);
        assert!(post > pre, "higher epoch dominates even with lower counter");
    }

    #[test]
    fn cursor_is_after_uses_total_order() {
        let a = Cursor::new(Seq::new(1, 5), EventId::from_u128(1));
        let b = Cursor::new(Seq::new(1, 6), EventId::from_u128(2));
        assert!(b.is_after(&a));
        assert!(!a.is_after(&b));
        assert!(!a.is_after(&a), "a cursor is never strictly after itself");
    }

    #[test]
    fn cursor_event_id_breaks_seq_ties() {
        let s = Seq::new(3, 3);
        let lo = Cursor::new(s, EventId::from_u128(1));
        let hi = Cursor::new(s, EventId::from_u128(2));
        assert!(lo.is_before(&hi));
        assert!(!hi.is_before(&lo));
    }
}
