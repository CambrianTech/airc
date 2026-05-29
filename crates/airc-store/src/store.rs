//! The `EventStore` trait — the consumer-facing API.
//!
//! Every backing (SQLite, Postgres, in-memory, mock) implements this
//! interface. The trait is `Send + Sync` so daemon code can wrap the
//! store in `Arc<dyn EventStore>` and hand it out across tasks.

use async_trait::async_trait;

use airc_core::{RoomId, TranscriptCursor, TranscriptEvent};

use crate::beacon::StoredBeacon;
use crate::error::StoreError;
use crate::local_identity::StoredLocalIdentity;
use crate::mesh_identity::StoredMeshIdentity;
use crate::refresh_lock::StoredRefreshLockOutcome;
use crate::subscriptions::StoredSubscription;

/// Durable transcript event store.
///
/// All operations are async — the backing may do disk / network I/O.
/// Cursor semantics follow `airc_core::cursor`: `(lamport, event_id)`
/// is the canonical position; lamport is the primary order, event_id
/// is the deterministic tiebreaker.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Load the default-agent local identity row, if present.
    ///
    /// Equivalent to [`Self::load_local_identity_by_agent_name`] with
    /// `"default"`. Kept for backwards compatibility with every
    /// pre-card-8384cc18 callsite that resolves identity without
    /// being multi-agent-aware. New code that knows its agent
    /// discriminator should call the by-name variant directly so
    /// `AIRC_AGENT_NAME` / `airc init --as <name>` (Sub-D, card
    /// 0f749d4c) can override it.
    async fn load_local_identity(&self) -> Result<Option<StoredLocalIdentity>, StoreError>;

    /// Load the local identity row whose `agent_name` matches.
    ///
    /// Card 8384cc18 Sub-C — the multi-agent read surface that
    /// Sub-B's schema (unique index on `agent_name`) enables. Returns
    /// `Ok(None)` when no row is named `agent_name` so callers can
    /// distinguish "fresh database for this agent" from a real I/O
    /// error and decide whether to generate vs. surface a partial
    /// state.
    ///
    /// Sub-D wires this through `AIRC_AGENT_NAME` and
    /// `airc init --as <name>`; until that ships, the default-agent
    /// path via [`Self::load_local_identity`] remains the entrypoint
    /// for every existing caller.
    async fn load_local_identity_by_agent_name(
        &self,
        agent_name: &str,
    ) -> Result<Option<StoredLocalIdentity>, StoreError>;

    /// Insert a local identity row.
    ///
    /// Implementations must fail if a row with the same
    /// `agent_name` already exists; changing peer/client identity
    /// is a new identity, not an update. Multiple rows with
    /// distinct `agent_name`s are legal post-Sub-B (8384cc18).
    async fn insert_local_identity(&self, identity: StoredLocalIdentity) -> Result<(), StoreError>;

    /// Replace only the user-facing identity card fields on the
    /// singleton row. Peer/client ids are immutable.
    async fn save_local_identity_card(
        &self,
        identity: airc_core::identity::Identity,
    ) -> Result<(), StoreError>;

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

    /// Try to acquire the account-registry refresh singleflight lock.
    ///
    /// Implementations must make the acquire/takeover decision
    /// atomically. If the existing lock is fresher than
    /// `refresh_interval_ms`, return `HeldFresh`; otherwise replace it
    /// with the caller's `(now_ms, holder_pid)` and return `Acquired`.
    async fn try_acquire_refresh_lock(
        &self,
        mesh_identity: &str,
        now_ms: u64,
        refresh_interval_ms: u64,
        holder_pid: u32,
    ) -> Result<StoredRefreshLockOutcome, StoreError>;

    /// Release the account-registry refresh lock. Idempotent.
    async fn release_refresh_lock(&self, mesh_identity: &str) -> Result<(), StoreError>;
}
