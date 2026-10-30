//! What a bounded history scan matches: the routing-header predicate, and
//! optionally the addressee pre-filter. One borrowed value so every store and
//! sink leg takes the same predicate.

use crate::{AddresseeFilter, Addressing, HeaderFilter, TranscriptEvent};

static EMPTY_KINDS: std::collections::BTreeSet<crate::TranscriptKind> =
    std::collections::BTreeSet::new();

#[derive(Debug, Clone)]
pub struct HistoryMatch<'a> {
    pub kinds: &'a std::collections::BTreeSet<crate::TranscriptKind>,
    pub self_echo: Option<(crate::PeerId, crate::ClientId, crate::SelfFilter)>,
    pub selection: crate::EventSelection,
    pub headers: &'a HeaderFilter,
    /// `None` matches regardless of addressing.
    pub addressed_to: Option<AddresseeFilter>,
}

impl<'a> HistoryMatch<'a> {
    pub fn headers(headers: &'a HeaderFilter) -> Self {
        Self {
            kinds: &EMPTY_KINDS,
            self_echo: None,
            selection: crate::EventSelection::All,
            headers,
            addressed_to: None,
        }
    }

    /// Whether an event whose addressing is computed on demand matches. The
    /// addressing is only computed when an addressee filter is set.
    pub fn matches_with(
        &self,
        class: crate::EventClass,
        kind: crate::TranscriptKind,
        from: (crate::PeerId, crate::ClientId),
        headers: &crate::Headers,
        addressing: impl FnOnce() -> Addressing,
    ) -> bool {
        self.selection.matches(class)
            && (self.kinds.is_empty() || self.kinds.contains(&kind))
            && self
                .self_echo
                .is_none_or(|(peer, client, filter)| match filter {
                    crate::SelfFilter::IncludeAll => true,
                    crate::SelfFilter::ExcludeSameClient => from.1 != client,
                    crate::SelfFilter::ExcludeSamePeer => from.0 != peer,
                })
            && self.headers.matches(headers)
            && self
                .addressed_to
                .is_none_or(|filter| addressing().admitted_by(&filter))
    }

    pub fn matches_event(&self, event: &TranscriptEvent) -> bool {
        self.matches_with(
            event.class,
            event.kind,
            (event.peer_id, event.client_id),
            &event.headers,
            || Addressing::of_event(event),
        )
    }
}
