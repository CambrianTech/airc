//! What a bounded history scan matches: the routing-header predicate, and
//! optionally the addressee pre-filter. One borrowed value so every store and
//! sink leg takes the same predicate.

use crate::{AddresseeFilter, Addressing, HeaderFilter, TranscriptEvent};

#[derive(Debug, Clone, Copy)]
pub struct HistoryMatch<'a> {
    pub headers: &'a HeaderFilter,
    /// `None` matches regardless of addressing.
    pub addressed_to: Option<AddresseeFilter>,
}

impl<'a> HistoryMatch<'a> {
    pub fn headers(headers: &'a HeaderFilter) -> Self {
        Self {
            headers,
            addressed_to: None,
        }
    }

    /// Whether an event whose addressing is computed on demand matches. The
    /// addressing is only computed when an addressee filter is set.
    pub fn matches_with(
        &self,
        headers: &crate::Headers,
        addressing: impl FnOnce() -> Addressing,
    ) -> bool {
        self.headers.matches(headers)
            && self
                .addressed_to
                .is_none_or(|filter| addressing().admitted_by(&filter))
    }

    pub fn matches_event(&self, event: &TranscriptEvent) -> bool {
        self.matches_with(&event.headers, || Addressing::of_event(event))
    }
}
