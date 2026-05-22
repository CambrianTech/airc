//! The `EventStore` trait — the consumer-facing API.
//!
//! Every backing (SQLite, Postgres, in-memory, mock) implements this
//! interface. The trait is `Send + Sync` so daemon code can wrap the
//! store in `Arc<dyn EventStore>` and hand it out across tasks.

use async_trait::async_trait;

use airc_core::{RoomId, TranscriptCursor, TranscriptEvent};

use crate::beacon::StoredBeacon;
use crate::error::StoreError;
use crate::mesh_identity::StoredMeshIdentity;
use crate::subscriptions::StoredSubscription;

/// Durable transcript event store.
///
/// All operations are async — the backing may do disk / network I/O.
/// Cursor semantics follow `airc_core::cursor`: `(lamport, event_id)`
/// is the canonical position; lamport is the primary order, event_id
/// is the deterministic tiebreaker.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Durably persist `event`. On success the event is visible to
    /// every subsequent `page_recent` / `resume_from` call against
    /// the same store handle and to any other handle pointing at the
    /// same backing.
    ///
    /// Returns `StoreError::DuplicateEventId` if an event with the
    /// same `event_id` was already appended (UUIDv4 makes this rare
    /// outside of explicit replay).
    async fn append(&self, event: TranscriptEvent) -> Result<(), StoreError>;

    /// Return the `limit` newest events, optionally filtered to a
    /// single `channel`. Events are returned oldest → newest within
    /// the page (so the caller iterates in transcript order).
    async fn page_recent(
        &self,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError>;

    /// Return up to `limit` events strictly *after* `cursor`,
    /// optionally filtered to a single `channel`. Events are
    /// returned in transcript order (lamport asc, event_id asc).
    ///
    /// This is the "give me what I haven't seen yet" call subscribers
    /// use to resume from disk after a restart or after the in-memory
    /// fan-out path missed a frame.
    async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError>;

    /// Return the cursor of the newest event in `channel` (or globally
    /// if `channel` is None), or `None` if the store has no matching
    /// events. Useful for "subscribe to new events from here on" —
    /// callers grab the latest cursor and `resume_from(it)`.
    async fn latest_cursor(
        &self,
        channel: Option<RoomId>,
    ) -> Result<Option<TranscriptCursor>, StoreError>;

    /// Return a named runtime consumer's checkpoint.
    async fn load_runtime_cursor(
        &self,
        consumer_id: &str,
    ) -> Result<Option<TranscriptCursor>, StoreError>;

    /// Persist a named runtime consumer's checkpoint.
    async fn save_runtime_cursor(
        &self,
        consumer_id: &str,
        cursor: &TranscriptCursor,
        updated_at_ms: u64,
    ) -> Result<(), StoreError>;

    /// Load joined-channel/default-channel state.
    async fn load_subscriptions(&self) -> Result<Vec<StoredSubscription>, StoreError>;

    /// Replace joined-channel/default-channel state with `rows`.
    ///
    /// Callers pass the complete subscription projection; the store
    /// owns the durable table and never mirrors this into sidecar
    /// files.
    async fn replace_subscriptions(&self, rows: Vec<StoredSubscription>) -> Result<(), StoreError>;

    /// Load the cached mesh identity row for `scope`, if present.
    async fn load_mesh_identity(
        &self,
        scope: &str,
    ) -> Result<Option<StoredMeshIdentity>, StoreError>;

    /// Upsert the cached mesh identity row for its `scope`.
    async fn save_mesh_identity(&self, entry: StoredMeshIdentity) -> Result<(), StoreError>;

    /// Load the caller's own account-mesh beacon for `mesh_identity`,
    /// if present.
    async fn load_beacon(
        &self,
        mesh_identity: &str,
        peer_id: airc_core::PeerId,
    ) -> Result<Option<StoredBeacon>, StoreError>;

    /// List all account-mesh beacons for `mesh_identity`.
    async fn list_beacons(&self, mesh_identity: &str) -> Result<Vec<StoredBeacon>, StoreError>;

    /// Upsert one account-mesh beacon and replace its channel set in
    /// the same transaction.
    async fn save_beacon(&self, beacon: StoredBeacon) -> Result<(), StoreError>;

    /// Delete account-mesh beacons for `mesh_identity`.
    async fn delete_beacons(
        &self,
        mesh_identity: &str,
        peer_ids: &[airc_core::PeerId],
    ) -> Result<usize, StoreError>;
}
