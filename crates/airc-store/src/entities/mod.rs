//! SeaORM entity definitions.
//!
//! Currently one table:
//!   - `events` — the canonical transcript log. One row per persisted
//!     `TranscriptEvent`. Indexes on `(channel_id, lamport, event_id)`
//!     keep `page_recent` and `resume_from` O(log n + page) instead
//!     of O(n).

pub mod event;
