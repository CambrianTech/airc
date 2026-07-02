//! SeaORM entity definitions.
//!
//! Tables:
//!   - `events` — the canonical transcript log. One row per persisted
//!     `TranscriptEvent`. Indexes on `(channel_id, lamport, event_id)`
//!     keep `page_recent` and `resume_from` O(log n + page) instead
//!     of O(n).
//!   - `bus_events` — the owner-core durable tier (§3.3). One row per
//!     persisted `airc_bus::Envelope` (a CLEAN schema, NOT
//!     `TranscriptEvent`). Composite index on
//!     `(room_id, epoch, counter, event_id)` — the generational cursor
//!     order — keeps `DurableSink::page` a single indexed range scan.
//!   - `runtime_cursors` — per-consumer replay cursors.
//!   - `peer_trust` / `peer_rotation_audit` — trust anchors and
//!     signed key-rotation audit rows.
//!   - `subscriptions` — joined channel/default-channel state.
//!   - `local_identity` — singleton metadata paired with the on-disk
//!     `identity.key`. Secret material stays on disk; this table
//!     holds the `peer_id` / `client_id` / version / created_at
//!     bookkeeping plus the user-facing identity card.
//!   - `mesh_identity` — cached account identity for room derivation.
//!   - `account_registry` + `account_registry_gist_sentinel` — local
//!     cache of the published cross-machine registry document and the
//!     per-mesh-identity gist-id sentinel that the gh adapter uses
//!     to recognize its own gist across publishes.
//!   - `beacons` / `beacon_channels` — account-mesh presence.
//!   - `refresh_locks` — store-backed singleflight for rare remote
//!     registry refreshes.
//!   - `scoped_state` — the generic key→JSON store scoped to a user, a
//!     room, or a `(user, room)` pair. Backs editable walls, room
//!     coordination state, per-person prefs, widget UI state, and the
//!     adaptive tool-menu cursor — all one table, differing only by
//!     `(scope_key, key)`.
pub mod account_registry;
pub mod beacon;
pub mod beacon_channel;
pub mod bus_event;
pub mod event;
pub mod local_identity;
pub mod mesh_identity;
pub mod peer_rotation_audit;
pub mod peer_trust;
pub mod refresh_lock;
pub mod runtime_cursor;
pub mod scoped_state;
pub mod subscription;
