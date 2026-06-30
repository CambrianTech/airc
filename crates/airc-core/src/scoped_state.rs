//! Generic scoped-state primitives — the storage-neutral typed layer
//! over the `scoped_state` key→JSON store (`airc_store::scoped_state`).
//!
//! airc already owns identity (the public bio card, [`crate::identity`]),
//! rooms, and a **shared room wall** (`Airc::publish_wall_post` /
//! `wall_posts` — event-sourced, broadcast, supersede-chain, open
//! consumer-defined categories). Scoped state is the *private/local*
//! sibling of that wall: free-form, scoped `key → JSON` a peer reads/writes
//! for itself, and **never broadcasts**.
//!
//! The split is deliberate (compression, not duplication):
//! - **Shared room documents** — a room's plan, coding instructions, the
//!   recipe, rules/agenda/rag posts — are broadcast, audited, and seen by
//!   every participant. Those live on the existing **wall**
//!   (`WallPostPublished`), keyed by category. A scoped-state `Room` entry
//!   is NOT that — it is one peer's *local* room-scoped cache.
//! - **Private per-peer state** the wall can't serve — prefs, "where was I
//!   last" / the tool-menu mode cursor, widget UI state (open tabs / rooms)
//!   — is high-churn, peer-private, and last-write-wins. A growing
//!   supersede log is the wrong shape for it; a durable LWW row is right.
//!   That is what this store is for.
//!
//! A consumer (continuum's WallSource) presents ONE unified scoped
//! grounding surface to a persona by reading the wall (shared) and scoped
//! state (private) underneath — the payoff Joel named is concise RAG:
//! explicit, scoped, budgetable grounding layers instead of an
//! undifferentiated dump. The substrate just uses the right primitive per
//! scope.
//!
//! Three scopes — a user, a room, or a `(user, room)` pair — encoded into
//! a single flat `scope_key` string so the durable store stays a clean
//! two-column `(scope_key, key)` table. **This module owns that encoding**
//! (the one logical decision per the compression principle); the store
//! documents the convention but defers the format to [`ScopeRef::scope_key`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PeerId, RoomId};

/// The well-known `scoped_state` key, under [`ScopeRef::User`], holding a
/// peer's latest published identity card (serialized [`crate::identity::Identity`]
/// JSON, LWW-versioned by the card's `emitted_at_ms`). The durable per-peer
/// identity index `peer_alias` / `peer_identity_card` / `room_roster` resolve
/// names from.
///
/// This is the ONE shared exception to the "scoped state is private/local"
/// rule above: a peer's identity card is observed off the broadcast
/// `IdentityPublished` event and is globally meaningful (every peer resolves
/// the same name for `User(peer)`). It lives here, next to [`ScopeRef`], as the
/// single source of truth shared by the writer (airc-lib's observe chokepoints)
/// and every reader, including the daemon's IPC `PeerIdentityCard` handler —
/// which serves an attached client's read out of the daemon's own index
/// (the local store of an attached scope never sees foreign peers' cards).
pub const PEER_IDENTITY_STATE_KEY: &str = "identity.card";

/// What a piece of scoped state is attached to.
///
/// The typed handle consumers pass to airc-lib; encodes to / decodes
/// from the flat `scope_key` the durable store indexes on. `Copy`
/// because it is two ids at most.
///
/// All three scopes are **private/local** — scoped state never
/// broadcasts. Shared, audited, broadcast room documents are the wall's
/// job (`Airc::publish_wall_post`), not this store's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScopeRef {
    /// Room-independent state about one peer — prefs, "my plan", open
    /// rooms / tabs / widget layout.
    User(PeerId),
    /// One peer's *local* room-scoped cache — a denormalized projection
    /// it keeps for this room. NOT the shared room wall (use
    /// `publish_wall_post` for plan / instructions / recipe that every
    /// participant must see).
    Room(RoomId),
    /// One peer's state within one room — "where was I last", the
    /// tool-menu mode cursor, private notes here.
    UserInRoom(PeerId, RoomId),
}

impl ScopeRef {
    /// The flat key the durable store indexes on:
    /// `user:<peer>` / `room:<room>` / `uir:<peer>:<room>`.
    ///
    /// The composite-PK leftmost prefix (`scope_key`) lets the store
    /// list every key under a scope as a range scan, so this encoding
    /// also defines the list granularity.
    pub fn scope_key(&self) -> String {
        match self {
            ScopeRef::User(peer) => format!("user:{peer}"),
            ScopeRef::Room(room) => format!("room:{room}"),
            ScopeRef::UserInRoom(peer, room) => format!("uir:{peer}:{room}"),
        }
    }

    /// Parse a `scope_key` produced by [`Self::scope_key`]. Round-trips
    /// exactly. Returns `None` on an unrecognized prefix or a malformed
    /// UUID — callers fail loud rather than guess a scope.
    pub fn parse(scope_key: &str) -> Option<Self> {
        // `uir:` is checked first; it shares no prefix with `user:`
        // (4th byte `r` vs `e`) so order is not load-bearing, only tidy.
        if let Some(rest) = scope_key.strip_prefix("uir:") {
            let (peer, room) = rest.split_once(':')?;
            Some(ScopeRef::UserInRoom(parse_peer(peer)?, parse_room(room)?))
        } else if let Some(rest) = scope_key.strip_prefix("user:") {
            Some(ScopeRef::User(parse_peer(rest)?))
        } else if let Some(rest) = scope_key.strip_prefix("room:") {
            Some(ScopeRef::Room(parse_room(rest)?))
        } else {
            None
        }
    }
}

fn parse_peer(s: &str) -> Option<PeerId> {
    Uuid::parse_str(s).ok().map(PeerId::from_uuid)
}

fn parse_room(s: &str) -> Option<RoomId> {
    Uuid::parse_str(s).ok().map(RoomId::from_uuid)
}

/// A typed scoped-state record — the storage-neutral domain view of one
/// `(scope_key, key) -> value_json` row. The persistence DTO
/// (`airc_store::StoredScopedState`) is structurally identical; airc-lib
/// bridges the two (the `updated_by` peer id flattens to a string in the
/// store).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedStateEntry {
    /// The flat scope encoding — see [`ScopeRef::scope_key`]. Use
    /// [`Self::scope`] to recover the typed [`ScopeRef`].
    pub scope_key: String,
    /// The state key within the scope (`instructions`, `recipe`, `plan`,
    /// `notes`, `tool.mode`, `prefs`, `ui.tabs`, …). Open string, never
    /// an enum-of-kinds.
    pub key: String,
    /// Opaque JSON the substrate never parses — a wall body, a plan, a
    /// cursor, widget UI state. Consumers own the shape.
    pub value_json: String,
    /// LWW version counter owned by the writing consumer; the store
    /// records it verbatim and never arbitrates.
    pub version: i64,
    /// LWW tiebreak — emission time in epoch millis.
    pub updated_at_ms: i64,
    /// The peer that wrote it, if known (system / anonymous writes leave
    /// it unset).
    pub updated_by: Option<PeerId>,
}

impl ScopedStateEntry {
    /// Build an entry from a typed [`ScopeRef`], encoding the scope_key.
    pub fn new(
        scope: ScopeRef,
        key: impl Into<String>,
        value_json: impl Into<String>,
        version: i64,
        updated_at_ms: i64,
        updated_by: Option<PeerId>,
    ) -> Self {
        Self {
            scope_key: scope.scope_key(),
            key: key.into(),
            value_json: value_json.into(),
            version,
            updated_at_ms,
            updated_by,
        }
    }

    /// Recover the typed [`ScopeRef`], or `None` if `scope_key` is
    /// malformed (a corrupt row — caller decides whether to skip it).
    pub fn scope(&self) -> Option<ScopeRef> {
        ScopeRef::parse(&self.scope_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the scope_key encoding and ScopeRef::parse must
    // be exact inverses for all three scopes — a drift here silently
    // mis-routes a room wall to a per-user key (or fails to find it).
    #[test]
    fn scope_key_round_trips_for_all_three_scopes() {
        let peer = PeerId::from_u128(0xA);
        let room = RoomId::from_u128(0xB);
        for scope in [
            ScopeRef::User(peer),
            ScopeRef::Room(room),
            ScopeRef::UserInRoom(peer, room),
        ] {
            let key = scope.scope_key();
            assert_eq!(ScopeRef::parse(&key), Some(scope), "round-trip {key}");
        }
        // The encoded prefixes are what the store range-scans on.
        assert_eq!(ScopeRef::User(peer).scope_key(), format!("user:{peer}"));
        assert_eq!(ScopeRef::Room(room).scope_key(), format!("room:{room}"));
        assert_eq!(
            ScopeRef::UserInRoom(peer, room).scope_key(),
            format!("uir:{peer}:{room}")
        );
    }

    // what this catches: parse must reject garbage rather than guess a
    // scope — fail-loud discipline, not a silent default.
    #[test]
    fn parse_rejects_unknown_prefix_and_bad_uuid() {
        assert_eq!(ScopeRef::parse("group:123"), None); // unknown prefix
        assert_eq!(ScopeRef::parse("user:not-a-uuid"), None); // bad uuid
        assert_eq!(ScopeRef::parse("uir:onlyone"), None); // missing room half
        assert_eq!(ScopeRef::parse(""), None);
    }

    // what this catches: scope() recovers the typed ScopeRef set by new(),
    // so a consumer reading a listed entry can route by scope.
    #[test]
    fn entry_new_sets_scope_key_and_scope_recovers_it() {
        let scope = ScopeRef::UserInRoom(PeerId::from_u128(4), RoomId::from_u128(5));
        let entry = ScopedStateEntry::new(scope, "tool.mode", "\"code\"", 1, 0, None);
        assert_eq!(entry.scope_key, scope.scope_key());
        assert_eq!(entry.scope(), Some(scope));
    }
}
