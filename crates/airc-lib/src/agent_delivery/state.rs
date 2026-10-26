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
/// - `addressed_below`: addressed requests at or after this position (and up to
///   `boundary`) have been emitted; the next page reads strictly below it.
///   `None`: none emitted yet in this range.
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
    pub(super) addressed_below: Option<TranscriptCursor>,
}
impl State {
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
