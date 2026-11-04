//! Matching durable history over envelopes already in memory: the one owner for the in-memory
//! sink and for a store's unsettled read, which loads its newest span by rowid and matches it
//! here, from each envelope, needing no derived data or index.

use airc_core::{HeaderFilter, HistoryCount, HistoryMatch, RoomId, RuntimeSelf};

use crate::envelope::{Cursor, Envelope};

fn in_range(e: &Envelope, channel: RoomId, after: &Cursor, through: &Cursor) -> bool {
    e.channel == channel
        && e.delivery.is_durable()
        && e.cursor().is_after(after)
        && !e.cursor().is_after(through)
}

/// The durable envelopes of `channel` in `(after, through]` matching `rule`: the first `limit`
/// in cursor order (the last `limit` for `tail`).
pub fn page<'a>(
    events: impl IntoIterator<Item = &'a Envelope>,
    channel: RoomId,
    after: Cursor,
    through: Cursor,
    rule: HistoryMatch<'_>,
    limit: usize,
    tail: bool,
) -> Vec<Envelope> {
    airc_core::bounded_selection::select(
        events
            .into_iter()
            .filter(|e| in_range(e, channel, &after, &through) && e.matches_history(rule)),
        |e| (e.seq, e.event_id.0),
        limit,
        tail,
    )
}

/// How many durable envelopes of `channel` in `(after, through]` match `headers`, without
/// `exclude`'s own sends: newest first, stopping past the cap like every store's count.
pub fn count<'a>(
    events: impl IntoIterator<Item = &'a Envelope>,
    channel: RoomId,
    after: Cursor,
    through: Cursor,
    headers: &HeaderFilter,
    exclude: Option<&RuntimeSelf>,
) -> HistoryCount {
    let mut matched: Vec<&Envelope> = events
        .into_iter()
        .filter(|e| {
            in_range(e, channel, &after, &through)
                && headers.matches(&e.headers)
                && !exclude.is_some_and(|rule| rule.is_self(&e.headers, e.from.1))
        })
        .collect();
    matched.sort_by_key(|e| std::cmp::Reverse((e.seq, e.event_id.0)));
    HistoryCount::of_newest(
        matched
            .into_iter()
            .map(|e| e.occurred_at_ms)
            .take(airc_core::HISTORY_COUNT_CAP as usize + 1),
    )
}
