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

    /// Stored numeric value is outside the domain accepted by the
    /// Rust contract. Refuse silent wraparound at the DB boundary.
    #[error("invalid stored value for {field}: {value}")]
    InvalidStoredValue { field: &'static str, value: i128 },

    /// Stored string value doesn't round-trip through the Rust enum
    /// it was supposed to come from. Card 34942ec1 Sub-A: trust tier
    /// is stored as a wire string; an unknown variant means version
    /// skew (a newer binary wrote a tier this binary doesn't know),
    /// which must surface honestly rather than silently downgrading
    /// to a default.
    #[error("invalid stored enum string for {column}: {value:?}")]
    InvalidStoredEnumString { column: &'static str, value: String },

    /// Expected row is absent.
    #[error("store row not found: {0}")]
    NotFound(&'static str),

    /// Peer trust conflict: a stored peer has a different public key.
    #[error(
        "peer {peer_id} is already enrolled with pubkey {stored_pubkey_b64}; cannot replace it with {attempted_pubkey_b64} without signed rotation"
    )]
    PeerPubkeyConflict {
        peer_id: airc_core::PeerId,
        stored_pubkey_b64: String,
        attempted_pubkey_b64: String,
    },

    /// Peer public key data is malformed in the store.
    #[error("peer pubkey is {0} bytes, expected 32")]
    WrongPubkeyLength(usize),

    /// Peer public key base64 is malformed in the store.
    #[error("peer pubkey base64: {0}")]
    Base64(#[from] base64::DecodeError),

    /// JSON encode/decode failure on the stored `metadata` /
    /// `headers` / `body` blob. Wrapped so callers can distinguish
    /// "corrupt persisted state" from a normal append failure.
    #[error("store payload codec error: {0}")]
    Codec(#[from] serde_json::Error),
}
