//! History before attachment: the store's count, the newest page, and the turn
//! that composes them with the addressed pages.

use airc_core::TranscriptEvent;

use super::addressed::AddressedWindow;
use super::state::Unsearched;
use super::{key, AgentDeliveryTurn};
use crate::AircError;

/// What a turn found between a consumer's settled position and its attachment.
/// Room history is counted by the store, never replayed; a request addressed to
/// this identity is never reduced to the count (card ba22c72c, and the
/// DeliverySelection contract that an authorized direct request is never
/// filtered out).
#[derive(Clone, Debug, PartialEq)]
pub struct HistorySummary {
    /// Matching events in the range, with their oldest and newest times; shown
    /// once per range (`None` on later turns that only page addressed requests).
    pub counted: Option<airc_core::HistoryCount>,
    /// The newest page of the range's room events, shown whole, oldest first,
    /// with the count (the way a chat client opens a channel on its newest
    /// page). Addressed requests are not repeated here; they page below.
    pub recent: Vec<TranscriptEvent>,
    /// One reader page of the events addressed to this identity, newest first.
    pub addressed: Vec<TranscriptEvent>,
    /// Addressed events already found beyond that page; later turns emit them.
    pub more_addressed: usize,
    /// Whether this page finishes the range: no addressed request is left
    /// unshown. While false, the history stays unsettled and later turns page on.
    pub addressed_complete: bool,
}

/// How far one summary turn got: whether it showed the count, and the
/// unsearched ranges left once its addressed page is emitted.
#[derive(Clone)]
pub(super) struct HistoryProgress {
    pub(super) counted_shown: bool,
    /// The unsearched ranges once this turn's page is emitted; empty: settle.
    pub(super) unsearched: Vec<Unsearched>,
}
impl AgentDeliveryTurn {
    /// One turn of the unsettled pre-attachment history. The ambient count is
    /// the store's (headers only, no event read) and is shown once per range,
    /// with the newest page of room events shown whole (one bounded newest-first
    /// store read of `window.page` events, never a walk);
    /// addressed requests are paged newest first by [`Self::addressed_history`]
    /// until every one is emitted, and only then does the range settle.
    /// `own_sends` is the ONE self-echo rule, applied to the count (translated by
    /// the store) and to the page. `None` when this turn has nothing to show;
    /// `acknowledge(.., true)` records the turn's progress either way.
    pub async fn history_summary(
        &mut self,
        filter: crate::EventFilter,
        own_sends: Option<&airc_core::RuntimeSelf>,
        keep: impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        window: AddressedWindow,
    ) -> Result<Option<HistorySummary>, AircError> {
        self.progress = None;
        if !self.has_history() {
            return Ok(None);
        }
        let counted = if self.state.history_counted {
            None
        } else {
            let count = self
                .airc
                .count_agent_feed_in_channels(
                    &self.state.summarized,
                    &self.state.boundary,
                    &self.channels,
                    filter.clone(),
                    own_sends,
                )
                .await?;
            (count.count > 0).then_some(count)
        };
        let shown = |e: &TranscriptEvent| keep(e) && !own_sends.is_some_and(|r| r.is_self_event(e));
        let recent = if counted.is_some() {
            self.recent_history(&filter, &shown, me, window.page)
                .await?
        } else {
            Vec::new()
        };
        let page = self.addressed_history(shown, me, window).await?;
        let addressed_complete = page.unsearched.is_empty();
        self.progress = Some(HistoryProgress {
            counted_shown: !self.state.history_counted,
            unsearched: page.unsearched,
        });
        if counted.is_none() && page.shown.is_empty() {
            return Ok(None);
        }
        Ok(Some(HistorySummary {
            counted,
            recent,
            addressed: page.shown,
            more_addressed: page.more,
            addressed_complete,
        }))
    }

    /// The newest `page` room events of the unsettled history that `shown` keeps,
    /// oldest first, leaving out requests addressed to `me` (those page on their
    /// own). One newest-first store read bounded by `page`; never a walk.
    async fn recent_history(
        &self,
        filter: &crate::EventFilter,
        shown: &impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        page: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let rule = airc_core::HistoryMatch {
            selection: filter.selection.clone(),
            headers: &filter.headers_filter,
            addressed_to: None,
            kinds: &filter.kinds,
            self_echo: filter.self_echo.map(|v| (v.peer_id, v.client_id, v.filter)),
        };
        let tail = self
            .airc
            .scan_matching_channels(
                None,
                &self.state.boundary,
                page.clamp(1, 4096),
                &self.channels,
                rule,
            )
            .await?;
        Ok(tail
            .events
            .into_iter()
            .filter(|e| key(&e.cursor()) > key(&self.state.summarized))
            .filter(|e| shown(e) && !me.is_addressed(e))
            .collect())
    }
}
