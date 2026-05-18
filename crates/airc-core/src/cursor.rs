//! Cursor + paging primitives for transcript fetch.
//!
//! A `TranscriptCursor` is the durable position into a transcript — the
//! lamport + event_id pair that uniquely orders events even when wall-
//! clock timestamps tie. Consumers paging history (scrollback, replay)
//! advance + rewind via cursors, never by file offsets or wall-clock.

use serde::{Deserialize, Serialize};

use crate::ids::{EventId, RoomId};
use crate::transcript::TranscriptEvent;

/// A position in a transcript. Two cursors compare deterministically via
/// `cursor_before` — lamport first, event_id as tiebreaker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptCursor {
    pub lamport: u64,
    pub event_id: EventId,
}

/// One page of transcript events for a single room, with cursors marking
/// the boundary of the slice so callers can page older / newer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub room_id: RoomId,
    pub events: Vec<TranscriptEvent>,
    /// Cursor of the newest event in this page — caller passes this to
    /// fetch events newer than the page.
    pub newer: Option<TranscriptCursor>,
    /// Cursor of the oldest event in this page — caller passes this to
    /// `page_before` to fetch the page before this one.
    pub older: Option<TranscriptCursor>,
}

/// Fetch the most recent `limit` events for the room, ordered.
pub fn page_recent(
    room_id: RoomId,
    events: &[TranscriptEvent],
    limit: usize,
) -> TranscriptPage {
    let mut page_events = events.to_vec();
    page_events.sort_by(event_order);
    if page_events.len() > limit {
        page_events = page_events[page_events.len() - limit..].to_vec();
    }
    page_for(room_id, page_events)
}

/// Fetch the most recent `limit` events strictly before the given cursor.
/// Used for scrollback — the caller has the oldest event of the current
/// view and wants the page before it.
pub fn page_before(
    room_id: RoomId,
    events: &[TranscriptEvent],
    before: &TranscriptCursor,
    limit: usize,
) -> TranscriptPage {
    let mut page_events: Vec<_> = events
        .iter()
        .filter(|event| cursor_before(&event.cursor(), before))
        .cloned()
        .collect();
    page_events.sort_by(event_order);
    if page_events.len() > limit {
        page_events = page_events[page_events.len() - limit..].to_vec();
    }
    page_for(room_id, page_events)
}

fn page_for(room_id: RoomId, events: Vec<TranscriptEvent>) -> TranscriptPage {
    TranscriptPage {
        room_id,
        newer: events.last().map(TranscriptEvent::cursor),
        older: events.first().map(TranscriptEvent::cursor),
        events,
    }
}

/// Cursor comparison — true iff `left` is strictly before `right` in
/// transcript order. Lamport first; event_id as tiebreaker.
fn cursor_before(left: &TranscriptCursor, right: &TranscriptCursor) -> bool {
    left.lamport < right.lamport
        || (left.lamport == right.lamport && left.event_id.0 < right.event_id.0)
}

/// Total event order: lamport first, then event_id alphabetically.
/// Public-but-not-API: callers should sort via `page_recent` / `page_before`
/// rather than reach in here.
pub(crate) fn event_order(
    left: &TranscriptEvent,
    right: &TranscriptEvent,
) -> std::cmp::Ordering {
    left.lamport
        .cmp(&right.lamport)
        .then_with(|| left.event_id.0.cmp(&right.event_id.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::SelfFilter;
    use crate::ids::{ClientId, PeerId};
    use crate::transcript::{MentionTarget, TranscriptKind};

    fn event(id: &str, lamport: u64, peer: &str, client: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId(id.to_string()),
            room_id: RoomId("general".to_string()),
            peer_id: PeerId(peer.to_string()),
            client_id: ClientId(client.to_string()),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            body: Some(format!("message {id}")),
            attachment: None,
            receipt: None,
            metadata: serde_json::json!({}),
        }
    }

    // Suppress unused warning from `SelfFilter` import used by sibling
    // crate::filter tests but not directly here.
    #[allow(dead_code)]
    const _: SelfFilter = SelfFilter::IncludeAll;

    #[test]
    fn recent_page_is_ordered_and_cursor_backed() {
        let events = vec![
            event("e3", 3, "a", "a1"),
            event("e1", 1, "a", "a1"),
            event("e2", 2, "b", "b1"),
        ];

        let page = page_recent(RoomId("general".to_string()), &events, 2);

        assert_eq!(
            page.events
                .iter()
                .map(|e| &e.event_id.0)
                .collect::<Vec<_>>(),
            vec!["e2", "e3"]
        );
        assert_eq!(page.older.unwrap().event_id.0, "e2");
        assert_eq!(page.newer.unwrap().event_id.0, "e3");
    }

    #[test]
    fn older_page_uses_cursor_not_file_tail() {
        let events = vec![
            event("e1", 1, "a", "a1"),
            event("e2", 2, "b", "b1"),
            event("e3", 3, "c", "c1"),
            event("e4", 4, "d", "d1"),
        ];

        let page = page_before(
            RoomId("general".to_string()),
            &events,
            &TranscriptCursor {
                lamport: 4,
                event_id: EventId("e4".to_string()),
            },
            2,
        );

        assert_eq!(
            page.events
                .iter()
                .map(|e| &e.event_id.0)
                .collect::<Vec<_>>(),
            vec!["e2", "e3"]
        );
    }
}
