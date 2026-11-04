//! Matching history over transcript events already in memory: the one owner for the in-memory
//! store and for an unsettled read's newest span (`crate::settling`), which loads its rows by
//! rowid and matches them here, from each event, needing no derived data or index.

use airc_core::{
    HeaderFilter, HistoryCount, HistoryMatch, RoomId, RuntimeSelf, TranscriptCursor,
    TranscriptEvent,
};

/// `event` lies strictly after `cursor` in transcript order (lamport, then event id).
pub(crate) fn strictly_after(event: &TranscriptEvent, cursor: &TranscriptCursor) -> bool {
    if event.lamport > cursor.lamport {
        return true;
    }
    if event.lamport == cursor.lamport {
        return event.event_id.as_uuid() > cursor.event_id.as_uuid();
    }
    false
}

fn in_range(
    event: &TranscriptEvent,
    after: &TranscriptCursor,
    through: &TranscriptCursor,
    channels: &[RoomId],
) -> bool {
    channels.contains(&event.room_id)
        && strictly_after(event, after)
        && !strictly_after(event, through)
}

/// The events of `channels` in `(after, through]` matching `rule`: the first `limit` in
/// transcript order (the last `limit` for `tail`).
pub(crate) fn page<'a>(
    events: impl IntoIterator<Item = &'a TranscriptEvent>,
    after: &TranscriptCursor,
    through: &TranscriptCursor,
    channels: &[RoomId],
    rule: HistoryMatch<'_>,
    limit: usize,
    tail: bool,
) -> Vec<TranscriptEvent> {
    airc_core::bounded_selection::select(
        events
            .into_iter()
            .filter(|e| in_range(e, after, through, channels) && rule.matches_event(e)),
        |e| (e.lamport, e.event_id.0),
        limit,
        tail,
    )
}

/// How many events of `channels` in `(after, through]` match `headers`, without `exclude`'s own.
pub(crate) fn count<'a>(
    events: impl IntoIterator<Item = &'a TranscriptEvent>,
    after: &TranscriptCursor,
    through: &TranscriptCursor,
    channels: &[RoomId],
    headers: &HeaderFilter,
    exclude: Option<&RuntimeSelf>,
) -> HistoryCount {
    events
        .into_iter()
        .filter(|e| {
            in_range(e, after, through, channels)
                && headers.matches(&e.headers)
                && !exclude.is_some_and(|rule| rule.is_self_event(e))
        })
        .fold(HistoryCount::default(), |count, e| {
            count.with_event(e.occurred_at_ms)
        })
}
