//! Paging the requests addressed to this identity out of pre-attachment
//! history, behind the checkpoint that keeps each one emitted exactly once.

use airc_core::TranscriptEvent;

use super::state::Unsearched;
use super::{key, AgentDeliveryTurn};
use crate::AircError;

pub(super) struct AddressedPage {
    pub(super) shown: Vec<TranscriptEvent>,
    pub(super) more: usize,
    /// The unsearched ranges once this page is emitted; empty: all emitted.
    pub(super) unsearched: Vec<Unsearched>,
}

/// How a turn looks for addressed history: how many of the newest events it may
/// read (the interim, see [`AgentDeliveryTurn::addressed_history`]) and how many
/// it shows.
#[derive(Clone, Copy, Debug)]
pub struct AddressedWindow {
    pub candidates: usize,
    pub page: usize,
}

impl AgentDeliveryTurn {
    /// The next page of requests addressed to `me` in the unsettled history,
    /// newest first, from the newest unsearched range on down.
    ///
    /// INTERIM until card 6fb2ea16 (Cormac): the store cannot yet select by
    /// addressee (the typed target is a JSON column and a leading `@name` lives
    /// in the body), so one turn reads at most `window.candidates` events in
    /// bounded tail scans, newest range first, and applies
    /// [`crate::Addressee`] to them. The range then shrinks only through what was
    /// actually emitted or scanned, so a request the bounded search has not
    /// reached is never settled past. When 6fb2ea16's indexed addressee query
    /// lands, this body becomes that query and callers do not change.
    pub(super) async fn addressed_history(
        &self,
        keep: impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        window: AddressedWindow,
    ) -> Result<AddressedPage, AircError> {
        let mut unsearched = self.state.unsearched.clone();
        let page = window.page.max(1);
        // One turn's work is bounded by `candidates` events in total; it moves on
        // to the next (older) range only while budget and page room remain, so
        // a reattach's small new segment costs no extra turn.
        let mut budget = window.candidates.max(1);
        let mut shown: Vec<TranscriptEvent> = Vec::new();
        let mut more = 0;
        while let Some(range) = unsearched.pop() {
            let room = page - shown.len();
            if budget == 0 || room == 0 {
                unsearched.push(range);
                break;
            }
            // An exclusive top reads one more and drops the top event itself.
            let limit = (budget + usize::from(range.top_exclusive)).min(4096);
            let tail = self
                .airc
                .scan_agent_feed_in_channels(None, &range.top, limit, &self.channels)
                .await?;
            let scanned: Vec<_> = tail
                .events
                .into_iter()
                .filter(|e| key(&e.cursor()) > key(&range.after))
                .filter(|e| !range.top_exclusive || key(&e.cursor()) < key(&range.top))
                .collect();
            let reached_start = scanned.len() < budget;
            budget -= scanned.len().min(budget);
            let oldest_scanned = scanned.first().map(|e| e.cursor());
            let mut found: Vec<_> = scanned
                .into_iter()
                .filter(|e| keep(e) && me.is_addressed(e))
                .collect();
            found.reverse();
            let take = found.len().min(room);
            let left_in_window = found.len() - take;
            found.truncate(take);
            // What of this range is left: below the oldest request shown when
            // more remain in the window; below the oldest event scanned when the
            // window held no more; nothing once the scan reached its start.
            let below = if left_in_window > 0 {
                found.last().map(|e| e.cursor())
            } else if reached_start {
                None
            } else {
                oldest_scanned
            };
            shown.extend(found);
            if let Some(top) = below {
                unsearched.push(Unsearched {
                    after: range.after,
                    top,
                    top_exclusive: true,
                });
                more = left_in_window;
                break;
            }
        }
        Ok(AddressedPage {
            shown,
            more,
            unsearched,
        })
    }
}
