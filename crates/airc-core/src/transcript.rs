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
    /// A peer published their identity card to a room — emitted on
    /// join (so other peers populate their roster on attach) and
    /// on nick / profile change. Body: serialized
    /// `airc_core::identity::IdentityEvent::PeerIdentityCard`,
    /// kind="peer_identity_card", carrying { peer_id, identity,
    /// emitted_at_ms }. Roster projections take the highest
    /// emitted_at_ms per peer_id. Part of card a63ad10a/2f74b8a1
    /// (identity-roster substrate, parent af40f46d).
    IdentityPublished,
    /// A peer published the room's operating doctrine — the
    /// "how we work here" markdown every attaching agent loads on
    /// join. Body: serialized
    /// `airc_core::doctrine::DoctrineEvent::RoomDoctrinePublished`,
    /// kind="room_doctrine_published", carrying { room_id, body,
    /// version, published_by, published_at_ms }. Projections take
    /// the latest per room_id (LWW on published_at_ms). Part of
    /// card 2903a8ef — engine keystone "the user is not the engine."
    DoctrinePublished,
    /// A peer pinned (or revised) a wall post — the room's living-
    /// document mechanism (card b4742d9c). Body: serialized
    /// `DoctrineEvent::WallPostPublished` carrying { room_id, post_id,
    /// category, body, supersedes, published_by, published_at_ms }.
    /// `category` is consumer-defined ("doctrine" / "rules" /
    /// "agenda" / "rag" / anything). Doctrine becomes the special
    /// case `category="doctrine"`; the legacy `DoctrinePublished`
    /// remains for back-compat replay.
    WallPostPublished,
    /// A peer published the room's TYPED purpose — the coarse kind
    /// (Chat / Coordination / Game / …) a citizen calibrates
    /// participation to WITHOUT parsing doctrine prose. Body: serialized
    /// `airc_core::channel_purpose::ChannelPurposeEvent::ChannelPurposePublished`,
    /// kind="channel_purpose_published", carrying { room_id, purpose,
    /// published_by, published_at_ms }. Projections take the latest per
    /// room_id (LWW on published_at_ms). Complementary to
    /// `DoctrinePublished` (typed kind here; free-form rules there).
    ChannelPurposePublished,
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
                | TranscriptKind::IdentityPublished
                | TranscriptKind::DoctrinePublished
                | TranscriptKind::WallPostPublished
                | TranscriptKind::ChannelPurposePublished
        )
    }

    /// True for the lifecycle kinds a REMOTE peer must receive — the announcements one
    /// citizen publishes *for the room*, as opposed to the observations a scope makes
    /// *about itself*.
    ///
    /// ## Why this is not `is_lifecycle`
    ///
    /// `is_lifecycle` answers "did the substrate author this, rather than a human typing",
    /// and it deliberately includes kinds that must NEVER leave the box: `WireEstablished`
    /// / `WireLost` are this scope's transport observations, `PeerArrived` / `PeerDeparted`
    /// are mutations of the LOCAL trust registry, `RoomJoined` / `RoomParted` are what THIS
    /// scope did, and `SubscriptionAdvanced` is a local consumer's cursor. "My wire came up"
    /// is nobody else's event. Using `is_lifecycle` as the routing predicate would broadcast
    /// every one of those.
    ///
    /// The four below are the opposite: each one's own doc says another peer consumes it —
    /// the identity card is emitted on join *"so other peers populate their roster on
    /// attach"*, the doctrine is *"the 'how we work here' markdown every attaching agent
    /// loads on join"*, the wall post is the room's living document, and the channel
    /// purpose is what *"a citizen calibrates participation to"*. A published announcement
    /// that reaches no peer has not been published.
    ///
    /// ## Why the decision lives on the KIND
    ///
    /// So it is made once. Measured on IntelMac 2026-09-05: every one of these four is
    /// emitted through `airc-lib`'s `emit_lifecycle`, which persists locally and sends to an
    /// in-process broadcast channel and never calls `router.publish` — so identity cards are
    /// structurally process-local. Three nodes each held a full LOCAL identity and rendered
    /// every peer as `peer-<hex>`, permanently and symmetrically, which is the signature of
    /// a reader looking at a store the writer never reaches rather than of propagation lag.
    /// Bolting a publish onto one call site would fix identity and leave the other three,
    /// which is how there came to be four of them.
    ///
    /// Adding a variant: answer this question at the definition site. The exhaustive match
    /// below makes the compiler ask.
    pub fn crosses_to_peers(self) -> bool {
        match self {
            // Announcements: published BY a citizen FOR the room.
            TranscriptKind::IdentityPublished
            | TranscriptKind::DoctrinePublished
            | TranscriptKind::WallPostPublished
            | TranscriptKind::ChannelPurposePublished => true,

            // This scope's own observations — local by nature.
            TranscriptKind::PeerArrived
            | TranscriptKind::PeerDeparted
            | TranscriptKind::WireEstablished
            | TranscriptKind::WireLost
            | TranscriptKind::RoomJoined
            | TranscriptKind::RoomParted
            | TranscriptKind::SubscriptionAdvanced => false,

            // Not lifecycle at all: these already ride their own send paths
            // (`say` / attachments / receipts), so routing them here would double-send.
            TranscriptKind::Message
            | TranscriptKind::Attachment
            | TranscriptKind::Receipt
            | TranscriptKind::Presence
            | TranscriptKind::SessionControl
            | TranscriptKind::System => false,
        }
    }

    /// Stable wire / storage discriminator string. This is the single
    /// source of truth for the `TranscriptKind ↔ &str` mapping that
    /// downstream codecs (airc-store SQLite, future JSON envelopes,
    /// debug rendering) consume.
    ///
    /// **NEVER rename an existing string after it ships** — these are
    /// persisted to SQLite and replayed on store open. Renaming a
    /// shipped variant is a schema migration, not a code change.
    ///
    /// Adding a variant: extend the match below AND
    /// [`Self::from_wire_str`] AND
    /// [`Self::ALL_VARIANTS`]. The compiler enforces the first via
    /// match exhaustiveness; the round-trip unit test
    /// (`wire_str_round_trip_covers_every_variant`) enforces the
    /// other two. Catching it here means no downstream consumer can
    /// silently drift, the way airc-store did when
    /// `IdentityPublished` landed (kink 0cfcc8db).
    pub fn as_wire_str(self) -> &'static str {
        match self {
            TranscriptKind::Message => "message",
            TranscriptKind::Attachment => "attachment",
            TranscriptKind::Receipt => "receipt",
            TranscriptKind::Presence => "presence",
            TranscriptKind::SessionControl => "session_control",
            TranscriptKind::System => "system",
            TranscriptKind::PeerArrived => "peer_arrived",
            TranscriptKind::PeerDeparted => "peer_departed",
            TranscriptKind::WireEstablished => "wire_established",
            TranscriptKind::WireLost => "wire_lost",
            TranscriptKind::RoomJoined => "room_joined",
            TranscriptKind::RoomParted => "room_parted",
            TranscriptKind::SubscriptionAdvanced => "subscription_advanced",
            TranscriptKind::IdentityPublished => "identity_published",
            TranscriptKind::DoctrinePublished => "doctrine_published",
            TranscriptKind::WallPostPublished => "wall_post_published",
            TranscriptKind::ChannelPurposePublished => "channel_purpose_published",
        }
    }

    /// Inverse of [`Self::as_wire_str`]. Returns `None` for unknown
    /// strings so callers can decide how to surface the error
    /// (`airc-store` wraps it in `StoreError::UnknownTranscriptKind`).
    pub fn from_wire_str(s: &str) -> Option<Self> {
        Some(match s {
            "message" => TranscriptKind::Message,
            "attachment" => TranscriptKind::Attachment,
            "receipt" => TranscriptKind::Receipt,
            "presence" => TranscriptKind::Presence,
            "session_control" => TranscriptKind::SessionControl,
            "system" => TranscriptKind::System,
            "peer_arrived" => TranscriptKind::PeerArrived,
            "peer_departed" => TranscriptKind::PeerDeparted,
            "wire_established" => TranscriptKind::WireEstablished,
            "wire_lost" => TranscriptKind::WireLost,
            "room_joined" => TranscriptKind::RoomJoined,
            "room_parted" => TranscriptKind::RoomParted,
            "subscription_advanced" => TranscriptKind::SubscriptionAdvanced,
            "identity_published" => TranscriptKind::IdentityPublished,
            "doctrine_published" => TranscriptKind::DoctrinePublished,
            "wall_post_published" => TranscriptKind::WallPostPublished,
            "channel_purpose_published" => TranscriptKind::ChannelPurposePublished,
            _ => return None,
        })
    }

    /// Every variant of [`TranscriptKind`], in declaration order. The
    /// round-trip test iterates this slice; adding a variant without
    /// extending this constant fails the test, which keeps
    /// `as_wire_str` / `from_wire_str` honest. This is the consumer-
    /// sync guard for the fan-out problem captured in card 0cfcc8db.
    pub const ALL_VARIANTS: &'static [TranscriptKind] = &[
        TranscriptKind::Message,
        TranscriptKind::Attachment,
        TranscriptKind::Receipt,
        TranscriptKind::Presence,
        TranscriptKind::SessionControl,
        TranscriptKind::System,
        TranscriptKind::PeerArrived,
        TranscriptKind::PeerDeparted,
        TranscriptKind::WireEstablished,
        TranscriptKind::WireLost,
        TranscriptKind::RoomJoined,
        TranscriptKind::RoomParted,
        TranscriptKind::SubscriptionAdvanced,
        TranscriptKind::IdentityPublished,
        TranscriptKind::DoctrinePublished,
        TranscriptKind::WallPostPublished,
        TranscriptKind::ChannelPurposePublished,
    ];
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Card 0cfcc8db. Every variant of `TranscriptKind` must
    /// appear in `ALL_VARIANTS`, and its `as_wire_str` discriminator
    /// must round-trip through `from_wire_str`. If a future variant
    /// extends the enum without extending the codec methods, this
    /// test fails inside `airc-core` — before any downstream
    /// consumer (airc-store SQLite codec, JSON envelopes, etc.) can
    /// silently drift the way airc-store did when `IdentityPublished`
    /// landed.
    ///
    /// Pairs with match-exhaustiveness on `as_wire_str`: the compiler
    /// catches a missing arm there, and this test catches a missing
    /// arm in `from_wire_str` or `ALL_VARIANTS`.
    #[test]
    fn wire_str_round_trip_covers_every_variant() {
        assert!(
            !TranscriptKind::ALL_VARIANTS.is_empty(),
            "ALL_VARIANTS must list every TranscriptKind"
        );

        let mut seen = std::collections::HashSet::new();
        for &kind in TranscriptKind::ALL_VARIANTS {
            let s = kind.as_wire_str();
            assert!(
                seen.insert(s),
                "wire-str discriminator {s:?} is duplicated across variants — \
                 each TranscriptKind needs a unique stable string"
            );
            assert!(
                !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "wire-str {s:?} must be snake_case ascii (persisted to SQLite)"
            );
            let decoded = TranscriptKind::from_wire_str(s).unwrap_or_else(|| {
                panic!(
                    "from_wire_str({s:?}) returned None — add an arm to from_wire_str \
                     when you add a TranscriptKind variant"
                )
            });
            assert_eq!(
                decoded, kind,
                "{kind:?}.as_wire_str() = {s:?}, but from_wire_str({s:?}) = {decoded:?}"
            );
        }

        assert_eq!(
            TranscriptKind::from_wire_str("not_a_real_kind"),
            None,
            "unknown discriminators must return None (so callers can wrap as an error)"
        );
    }

    // what this catches: an announcement kind that silently stops crossing the wire, and a
    // local observation that starts crossing. Measured 2026-09-05: all four announcements
    // were emitted through a path that never reached the bus, so three nodes each rendered
    // every peer as `peer-<hex>` forever while holding a full local identity. The bug was
    // invisible because each half worked — the card was written, and the reader read; they
    // just used different stores. Pinning the SET here means the next kind added has to
    // answer the question at its definition site instead of inheriting silence.
    #[test]
    fn only_the_room_announcements_cross_to_peers() {
        let crossing: Vec<&str> = TranscriptKind::ALL_VARIANTS
            .iter()
            .filter(|k| k.crosses_to_peers())
            .map(|k| k.as_wire_str())
            .collect();
        assert_eq!(
            crossing,
            vec![
                "identity_published",
                "doctrine_published",
                "wall_post_published",
                "channel_purpose_published",
            ],
            "the set of kinds a remote peer receives changed — if that is deliberate, say \
             why here; `is_lifecycle` is NOT this set and must not be substituted for it"
        );
    }

    // what this catches: substituting `is_lifecycle` for the routing predicate. It is the
    // tempting one-liner and it would broadcast this scope's own transport and registry
    // observations to every peer in the room — "my wire came up" is nobody else's event.
    #[test]
    fn a_scopes_own_observations_never_cross() {
        for local in [
            TranscriptKind::PeerArrived,
            TranscriptKind::PeerDeparted,
            TranscriptKind::WireEstablished,
            TranscriptKind::WireLost,
            TranscriptKind::RoomJoined,
            TranscriptKind::RoomParted,
            TranscriptKind::SubscriptionAdvanced,
        ] {
            assert!(local.is_lifecycle(), "{local:?} should still be lifecycle");
            assert!(
                !local.crosses_to_peers(),
                "{local:?} is this scope's own observation and must not be broadcast"
            );
        }
    }
}
