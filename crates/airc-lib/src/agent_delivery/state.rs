//! One consumer's persisted delivery state and its cursor rules.

use airc_core::{RoomId, TranscriptCursor};
use serde::{Deserialize, Serialize};

use super::key;

/// One consumer's persisted delivery state. Each position has ONE meaning:
/// - `live`: the DELIVERY proof, the end of the live range this consumer flushed
///   and acknowledged. Only `acknowledge(live_end, ..)` moves it, never an
///   attach: a reattach that is dropped before output leaves it where it was.
/// - `boundary`: where the current attachment started reading live. Events at or
///   before it are history; the live scan starts at `max(live, boundary)`.
/// - `summarized`: history settled through here (counted, and every addressed
///   request in it emitted). Moves only when a summary turn finishes the range.
/// - `history_counted`: the ambient count of `(summarized, boundary]` was shown.
/// - `unsearched`: the stretches of `(summarized, boundary]` whose addressed
///   requests are not yet all emitted, oldest first. A turn serves the oldest
///   one first (FIFO, so new addressed traffic cannot starve an older request),
///   paging each down from its top; an unsettled reattach appends the new top
///   segment rather than losing it. Nothing is ever reset, so no shown request is emitted twice, and the
///   range settles only when this list is empty.
///
/// Settling a summary marks nothing delivered, delivering marks no history
/// summarized, and no settlement buries an addressed request not yet emitted.
/// `summarized` keeps its original persisted key, `backlog`, so existing rows load.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct State {
    pub(super) version: u32,
    pub(super) boundary: TranscriptCursor,
    #[serde(rename = "backlog")]
    pub(super) summarized: TranscriptCursor,
    pub(super) live: TranscriptCursor,
    pub(super) rooms: Vec<RoomId>,
    pub(super) source: String,
    #[serde(default)]
    pub(super) history_counted: bool,
    #[serde(default)]
    pub(super) unsearched: Vec<Unsearched>,
}

/// Pre-attachment history whose addressed requests are not all emitted yet:
/// events after `after`, up to `top` (inclusive unless `top_exclusive`, which a
/// page sets so the next one reads strictly below what it already showed).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Unsearched {
    pub(super) after: TranscriptCursor,
    pub(super) top: TranscriptCursor,
    pub(super) top_exclusive: bool,
}
impl State {
    /// The whole unsettled history as one unsearched range, for a state that has
    /// none recorded (a new consumer, or a row written before ranges existed).
    pub(super) fn ensure_unsearched(&mut self) {
        if key(&self.summarized) < key(&self.boundary) && self.unsearched.is_empty() {
            self.unsearched.push(Unsearched {
                after: self.summarized.clone(),
                top: self.boundary.clone(),
                top_exclusive: false,
            });
        }
    }

    /// Where live delivery reads from: after both what was delivered and where
    /// this attachment began.
    pub(super) fn scan_from(&self) -> &TranscriptCursor {
        if key(&self.live) > key(&self.boundary) {
            &self.live
        } else {
            &self.boundary
        }
    }
}
