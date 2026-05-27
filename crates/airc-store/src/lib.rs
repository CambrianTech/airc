//! `airc-store` — durable ORM store for AIRC runtime data.
//!
//! Closes grievance §5 (CLI/Daemon Is Accumulating Policy — the
//! needed crate split lists `airc-store` as the source of truth for
//! runtime data) and §7 (Inbox And Replay Need Stronger Cursor
//! Semantics — the store owns `(lamport, event_id)` cursoring and
//! channel-aware filtering rather than the per-wire JSONL append in
//! airc-transport).
//!
//! The trait surface ([`EventStore`]) is the consumer-facing API:
//!   - `append(event)` durably persists a `TranscriptEvent`;
//!   - `page_recent(channel, limit)` returns the newest N events;
//!   - `resume_from(cursor, channel, limit)` returns events strictly
//!     after the cursor;
//!   - `latest_cursor(channel)` returns the newest cursor or None.
//!   - peer trust tables hold enrolled peer keys and signed rotation
//!     audit rows.
//!
//! Two implementations ship in this crate:
//!   - [`SqliteEventStore`]: SeaORM-backed SQLite. Production target
//!     for v1; migrations applied on `open`.
//!   - [`InMemoryEventStore`]: trait-only test double, no I/O. Use
//!     in unit tests that don't need durability.
//!
//! The owner-core durable tier ships here too: [`SqliteDurableSink`]
//! is a SeaORM-backed SQLite implementation of `airc_bus::DurableSink`
//! (§3.3 of `docs/architecture/AIRC-EVENT-SERVER.md`) — the real
//! persistence behind the bus's `Durable` envelopes, replacing the
//! in-memory test sink. `airc-store` depends on `airc-bus` (the lower-
//! level generic crate), never the reverse.

#![deny(unsafe_code)]
// `rust_2018_idioms` would force `&SchemaManager<'_>` syntax on the
// MigrationTrait impls, but sea-orm-migration declares those methods
// late-bound (`&SchemaManager` without a lifetime param) — the impl
// signature has to match verbatim. Opt out at the crate level rather
// than scattering `#[allow(elided_lifetimes_in_paths)]` per impl.

pub mod account_registry;
pub mod beacon;
pub mod bus_epoch_store;
pub mod bus_sink;
pub mod entities;
pub mod error;
pub mod local_identity;
pub mod memory;
pub mod mesh_identity;
pub mod migration;
pub mod peer_trust;
pub mod refresh_lock;
pub mod sqlite;
pub mod store;
pub mod subscriptions;

pub use account_registry::{StoredAccountRegistry, StoredAccountRegistryGistSentinel};
pub use beacon::StoredBeacon;
pub use bus_epoch_store::SqliteEpochStore;
pub use bus_sink::SqliteDurableSink;
pub use error::StoreError;
pub use local_identity::StoredLocalIdentity;
pub use memory::InMemoryEventStore;
pub use mesh_identity::StoredMeshIdentity;
pub use peer_trust::{RotationAuditEntry, StoredPeer};
pub use refresh_lock::{StoredRefreshLock, StoredRefreshLockOutcome};
pub use sqlite::SqliteEventStore;
pub use store::EventStore;
pub use subscriptions::StoredSubscription;
