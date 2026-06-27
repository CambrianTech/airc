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
pub fn page_recent(room_id: RoomId, events: &[TranscriptEvent], limit: usize) -> TranscriptPage {
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
pub(crate) fn event_order(left: &TranscriptEvent, right: &TranscriptEvent) -> std::cmp::Ordering {
    left.lamport
        .cmp(&right.lamport)
        .then_with(|| left.event_id.0.cmp(&right.event_id.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::Body;
    use crate::ids::{ClientId, PeerId};
    use crate::transcript::{MentionTarget, TranscriptKind};

    /// Test helper: construct a transcript event with deterministic
    /// UUID ids derived from the lamport. Deterministic so test
    /// assertions are stable across runs.
    fn event_at(lamport: u64, peer_seed: u128, client_seed: u128) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::from_u128(lamport as u128),
            room_id: RoomId::from_u128(0xc0ffee),
            peer_id: PeerId::from_u128(peer_seed),
            client_id: ClientId::from_u128(client_seed),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            headers: Default::default(),
            body: Some(Body::text(format!("message at lamport {lamport}"))),
            attachment: None,
            receipt: None,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn recent_page_is_ordered_and_cursor_backed() {
        let e1 = event_at(1, 0xaa, 0xa1);
        let e2 = event_at(2, 0xbb, 0xb1);
        let e3 = event_at(3, 0xaa, 0xa1);
        let room = RoomId::from_u128(0xc0ffee);
        let events = vec![e3.clone(), e1.clone(), e2.clone()];

        let page = page_recent(room, &events, 2);

        let ids: Vec<_> = page.events.iter().map(|e| e.event_id).collect();
        assert_eq!(ids, vec![e2.event_id, e3.event_id]);
        assert_eq!(page.older.unwrap().event_id, e2.event_id);
        assert_eq!(page.newer.unwrap().event_id, e3.event_id);
    }

    #[test]
    fn older_page_uses_cursor_not_file_tail() {
        let e1 = event_at(1, 0xaa, 0xa1);
        let e2 = event_at(2, 0xbb, 0xb1);
        let e3 = event_at(3, 0xcc, 0xc1);
        let e4 = event_at(4, 0xdd, 0xd1);
        let room = RoomId::from_u128(0xc0ffee);
        let events = vec![e1, e2.clone(), e3.clone(), e4.clone()];

        let page = page_before(
            room,
            &events,
            &TranscriptCursor {
                lamport: 4,
                event_id: e4.event_id,
            },
            2,
        );

        let ids: Vec<_> = page.events.iter().map(|e| e.event_id).collect();
        assert_eq!(ids, vec![e2.event_id, e3.event_id]);
    }
}
