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

pub mod event;
pub mod peer_rotation_audit;
pub mod peer_trust;
pub mod runtime_cursor;
