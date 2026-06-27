//! `bus_events` table — the durable tier for the owner-core
//! `airc_bus::Envelope` (§3.3 ORM durable tier of
//! `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! This is a CLEAN schema for the generic owner-core envelope — it is
//! **not** `TranscriptEvent` (`Envelope != TranscriptEvent`). The
//! `events` table mirrors the chat-shaped `TranscriptEvent`; this one
//! mirrors the generic envelope (opaque `payload` blob, generational
//! `seq = (epoch, counter)`, `Target`/`Kind`/`DeliveryClass` enums).
//!
//! ## Order key (NOT a single lamport)
//!
//! The owner-core total order is the **generational cursor**
//! `(epoch, counter, event_id)` (§3.5, §3.8) — `epoch` is bumped on
//! every daemon start so a post-crash event sorts strictly after a
//! pre-crash one even when the in-memory counter rewinds. The composite
//! index `idx_bus_events_room_epoch_counter_event_id` covers exactly
//! that order so `page` is a single indexed B-tree range scan, never a
//! full-table scan.
//!
//! Primary key: `event_id` (a UUIDv4) — append is an idempotent
//! ON CONFLICT DO NOTHING insert on it (a replay / re-inject must not
//! double-store, §3.3).

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "bus_events")]
pub struct Model {
    /// `event_id` as UUIDv4 — stable across replay, primary key. Append
    /// is idempotent on this column.
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: Uuid,
    /// Channel (room/stream) this envelope belongs to.
    pub room_id: Uuid,
    /// Generational order high half (`seq.epoch`). Bumped every daemon
    /// start so post-crash events sort after pre-crash (§3.8).
    pub epoch: i64,
    /// Generational order low half (`seq.counter`) — the in-memory
    /// monotonic counter within an epoch.
    pub counter: i64,
    /// `Kind` serialised as snake_case text (self-documenting; no enum
    /// mapping table migration when a kind is added).
    pub kind: String,
    /// `DeliveryClass` serialised as snake_case text. Only `durable`
    /// rows ever reach this table (§3.3 efficiency keystone).
    pub delivery: String,
    /// `Target` as JSON (`All` / `Endpoint` / `Peer` / `Reply` /
    /// `Capability`). Stored as TEXT JSON; SQLite is opinion-free.
    pub target: Json,
    /// Command ↔ result / request ↔ response correlation (nullable).
    pub correlation_id: Option<Uuid>,
    /// Coalescing key for `EphemeralLatest` — carried for completeness;
    /// `EphemeralLatest` is never persisted, so this is effectively
    /// always null for `durable` rows (nullable).
    pub coalesce_key: Option<String>,
    /// Routable metadata (`Headers` = `BTreeMap<String,String>`) as JSON.
    pub headers: Json,
    /// OPAQUE consumer payload bytes. Stored as a BLOB; airc never
    /// parses it.
    pub payload: Vec<u8>,
    /// Sender peer identity.
    pub peer_id: Uuid,
    /// Sender session identity.
    pub client_id: Uuid,
    /// Owner-stamped wall clock (ms).
    pub occurred_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
