//! Generic scoped-state store types.
//!
//! A single durable key→JSON bag, scoped to a user, a room, or a
//! `(user, room)` pair. This is the *internal* sibling of the identity
//! bio card: editable walls (a room's instructions or recipe), the
//! coordination layer (a room's shared plan), per-person state, widget
//! UI state (open tabs / open rooms / layout), and small cursors (the
//! adaptive tool-menu mode) all live here, differing only by
//! `scope_key` + `key`.
//!
//! The store is deliberately dumb, exactly like `account_registry`'s
//! `document_json`: `value_json` is opaque (a serialized
//! `serde_json::Value`) and the store never parses it, enforces a size
//! policy, or arbitrates `version`. Those are consumer concerns —
//! continuum reads these rows as a `RagSource` under its own RAG
//! budget; humans edit them as walls in a widget. One store, two faces.

/// One scoped-state row: `(scope_key, key) -> value_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredScopedState {
    /// Canonical scope encoding — `user:<peer>`, `room:<room>`, or
    /// `uir:<peer>:<room>`. A flat string rather than a nullable
    /// `(scope_kind, user_id?, room_id?)` triple so the primary key
    /// stays a clean two-column `(scope_key, key)`, mirroring the
    /// `account_registry` String-PK shape. The encoding lives with the
    /// domain `ScopeRef` type (airc-core, a later slice); the store
    /// only ever sees the already-encoded string.
    pub scope_key: String,
    /// The entry name within the scope — e.g. `instructions`, `recipe`,
    /// `plan`, `notes`, `tool.mode`, `prefs`, `ui.tabs`. Free-form so a
    /// recipe-defined activity can claim whatever key it needs without
    /// an airc schema change.
    pub key: String,
    /// Opaque serialized JSON value. The store never parses it.
    pub value_json: String,
    /// Monotonic last-write-wins counter owned by the writer. The store
    /// records it but does not arbitrate on it — a consumer that wants
    /// LWW-by-version reads, compares, and only writes the winner.
    pub version: i64,
    /// Wall-clock write time (ms). LWW tiebreak for equal versions.
    pub updated_at_ms: i64,
    /// Peer that authored this write — provenance for coordination
    /// (room-scope) writes. `None` for anonymous / system writes.
    pub updated_by: Option<String>,
}
