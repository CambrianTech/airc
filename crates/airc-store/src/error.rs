//! Typed store failures. Distinguishes I/O / driver errors from
//! consumer-induced problems (duplicate event_id, etc.) so callers
//! can react appropriately.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    /// Local filesystem failure while preparing the database path.
    #[error("store I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Underlying database / driver error.
    #[error("store database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    /// In-memory store lock was poisoned by a panic in another task.
    #[error("store lock poisoned")]
    LockPoisoned,

    /// Migration error during `open`.
    #[error("store migration error: {0}")]
    Migration(String),

    /// Tried to append an event whose `event_id` already exists.
    /// EventId is a UUIDv4 so honest senders won't collide; this
    /// generally indicates a replay or a duplicated network frame.
    #[error("duplicate event_id: {0}")]
    DuplicateEventId(uuid::Uuid),

    /// Stored transcript kind is not known to this binary. Refuse to
    /// reinterpret it as another kind; replay must be exact.
    #[error("unknown transcript kind: {0}")]
    UnknownTranscriptKind(String),

    /// JSON encode/decode failure on the stored `metadata` /
    /// `headers` / `body` blob. Wrapped so callers can distinguish
    /// "corrupt persisted state" from a normal append failure.
    #[error("store payload codec error: {0}")]
    Codec(#[from] serde_json::Error),
}
