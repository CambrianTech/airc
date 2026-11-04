//! What a stored event of a kind is worth once it has landed, declared ONCE per kind at
//! its definition ([`crate::TranscriptKind::retention`]) and read by the one drain in
//! `airc-store` (`retention`). There is no per-kind drain code: a kind gets its drain by
//! being defined, and a kind without a declaration does not compile (the declaration is an
//! exhaustive match). Joel, 2026-11-03: "Airc needs low level default drain behavior not a
//! million drains."

/// How long a stored row of a kind stays worth keeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Nothing reads a stored row of this kind: every row is dead once written.
    DeadOnArrival,
    /// Only the newest row per key is ever read (a last-writer-wins projection), so a row
    /// with a newer one of the same kind and key is dead.
    NewestPer(RetentionKey),
    /// The room's history: kept, and trimmed oldest first only under the store's byte
    /// budget.
    History,
}

/// The key a [`Retention::NewestPer`] kind is superseded within, as stored columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionKey {
    /// One live row per room.
    Room,
    /// One live row per peer in each room.
    RoomAndPeer,
}
