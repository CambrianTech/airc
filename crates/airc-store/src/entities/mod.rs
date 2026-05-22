//! SeaORM entity definitions.
//!
//! Tables:
//!   - `events` — the canonical transcript log. One row per persisted
//!     `TranscriptEvent`. Indexes on `(channel_id, lamport, event_id)`
//!     keep `page_recent` and `resume_from` O(log n + page) instead
//!     of O(n).
//!   - `runtime_cursors` — per-consumer replay cursors.
//!   - `peer_trust` / `peer_rotation_audit` — trust anchors and
//!     signed key-rotation audit rows.
//!   - `subscriptions` — joined channel/default-channel state.
//!   - `local_identity` — singleton metadata paired with the on-disk
//!     `identity.key`. Secret material stays on disk; this table
//!     holds the `peer_id` / `client_id` / version / created_at
//!     bookkeeping that lived in `identity.json` before Phase 3.5.
//!   - `mesh_identity` — cached account identity for room derivation.
//!   - `account_registry` + `account_registry_gist_sentinel` — local
//!     cache of the published cross-machine registry document and the
//!     per-mesh-identity gist-id sentinel that the gh adapter uses
//!     to recognize its own gist across publishes.

pub mod account_registry;
pub mod event;
pub mod local_identity;
pub mod mesh_identity;
pub mod peer_rotation_audit;
pub mod peer_trust;
pub mod runtime_cursor;
pub mod subscription;
