//! Paging the requests addressed to this identity out of pre-attachment
//! history, behind the checkpoint that keeps each one emitted exactly once.

use airc_core::{TranscriptCursor, TranscriptEvent};

use super::{key, AgentDeliveryTurn};
use crate::AircError;

pub(super) struct AddressedPage {
    pub(super) shown: Vec<TranscriptEvent>,
    pub(super) more: usize,
    pub(super) progress: AddressedProgress,
}

/// How a turn looks for addressed history: how many of the newest events it may
/// read (the interim, see [`AgentDeliveryTurn::addressed_history`]) and how many
/// it shows.
#[derive(Clone, Copy, Debug)]
pub struct AddressedWindow {
    pub candidates: usize,
    pub page: usize,
}

#[derive(Clone)]
pub(super) enum AddressedProgress {
    /// Every addressed request in the range is emitted: the range settles.
    Finished,
    /// Addressed requests at or after this position are emitted (or searched).
    Below(TranscriptCursor),
}
impl AgentDeliveryTurn {
    /// The next page of requests addressed to `me` in the unsettled history,
    /// newest first, strictly below what earlier turns emitted.
    ///
    /// INTERIM until card 6fb2ea16 (Cormac): the store cannot yet select by
    /// addressee (the typed target is a JSON column and a leading `@name` lives
    /// in the body), so one turn reads only the newest `window.candidates`
    /// events below the checkpoint, one bounded tail scan, and applies
    /// [`crate::Addressee`] to them. The checkpoint then moves down only through
    /// what was actually emitted or searched, so a request the bounded search
    /// has not reached is never settled past. When 6fb2ea16's indexed addressee
    /// query lands, this body becomes that query and callers do not change.
    pub(super) async fn addressed_history(
        &self,
        keep: impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        window: AddressedWindow,
    ) -> Result<AddressedPage, AircError> {
        let after = &self.state.summarized;
        let candidates = window.candidates.max(1);
        // Inclusive upper bound for the scan; an exclusive checkpoint reads one
        // more and drops the checkpoint event itself.
        let (upper, exclusive) = match &self.state.addressed_below {
            Some(below) => (below, true),
            None => (&self.state.boundary, false),
        };
        let limit = (candidates + usize::from(exclusive)).min(4096);
        let tail = self
            .airc
            .scan_agent_feed_in_channels(None, upper, limit, &self.channels)
            .await?;
        let scanned: Vec<_> = tail
            .events
            .into_iter()
            .filter(|e| key(&e.cursor()) > key(after))
            .filter(|e| !exclusive || key(&e.cursor()) < key(upper))
            .collect();
        let reached_start = scanned.len() < candidates;
        let oldest_scanned = scanned.first().map(|e| e.cursor());
        let mut found: Vec<_> = scanned
            .into_iter()
            .filter(|e| keep(e) && me.is_addressed(e))
            .collect();
        found.reverse();
        let page = window.page.max(1);
        let more = found.len().saturating_sub(page);
        found.truncate(page);
        let progress = if more > 0 {
            // Unshown requests remain in this window: resume just below the
            // oldest one emitted.
            match found.last() {
                Some(oldest_shown) => AddressedProgress::Below(oldest_shown.cursor()),
                None => AddressedProgress::Below(upper.clone()),
            }
        } else if reached_start {
            AddressedProgress::Finished
        } else {
            match oldest_scanned {
                Some(cursor) => AddressedProgress::Below(cursor),
                None => AddressedProgress::Finished,
            }
        };
        Ok(AddressedPage {
            shown: found,
            more,
            progress,
        })
    }
}
