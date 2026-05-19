//! Typed store failures. Distinguishes I/O / driver errors from
//! consumer-induced problems (duplicate event_id, etc.) so callers
//! can react appropriately.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    /// Underlying database / driver error.
    #[error("store database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    /// Migration error during `open`.
    #[error("store migration error: {0}")]
    Migration(String),

    /// Tried to append an event whose `event_id` already exists.
    /// EventId is a UUIDv4 so honest senders won't collide; this
    /// generally indicates a replay or a duplicated network frame.
    #[error("duplicate event_id: {0}")]
    DuplicateEventId(uuid::Uuid),

    /// JSON encode/decode failure on the stored `metadata` /
    /// `headers` / `body` blob. Wrapped so callers can distinguish
    /// "corrupt persisted state" from a normal append failure.
    #[error("store payload codec error: {0}")]
    Codec(#[from] serde_json::Error),
}
