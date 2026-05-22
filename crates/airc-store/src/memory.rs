//! In-memory `EventStore` — useful for tests and for tooling that
//! wants the trait shape without touching disk. Not durable; loses
//! all state when dropped.

use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Mutex;

use airc_core::{RoomId, TranscriptCursor, TranscriptEvent};

use crate::error::StoreError;
use crate::store::EventStore;
use crate::subscriptions::StoredSubscription;

pub struct InMemoryEventStore {
    events: Mutex<Vec<TranscriptEvent>>,
    runtime_cursors: Mutex<BTreeMap<String, TranscriptCursor>>,
    subscriptions: Mutex<Vec<StoredSubscription>>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            runtime_cursors: Mutex::new(BTreeMap::new()),
            subscriptions: Mutex::new(Vec::new()),
        }
    }
}

impl Default for InMemoryEventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventStore for InMemoryEventStore {
    async fn append(&self, ev: TranscriptEvent) -> Result<(), StoreError> {
        let mut events = self.events.lock().map_err(|_| StoreError::LockPoisoned)?;
        if events.iter().any(|e| e.event_id == ev.event_id) {
            return Err(StoreError::DuplicateEventId(ev.event_id.as_uuid()));
        }
        events.push(ev);
        Ok(())
    }

    async fn page_recent(
        &self,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
        let events = self.events.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut filtered: Vec<TranscriptEvent> = events
            .iter()
            .filter(|e| channel.is_none_or(|room| e.room_id == room))
            .cloned()
            .collect();
        filtered.sort_by(transcript_order);
        if filtered.len() > limit {
            let drop_count = filtered.len() - limit;
            filtered.drain(..drop_count);
        }
        Ok(filtered)
    }

    async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
        let events = self.events.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut filtered: Vec<TranscriptEvent> = events
            .iter()
            .filter(|e| channel.is_none_or(|room| e.room_id == room))
            .filter(|e| strictly_after(e, cursor))
            .cloned()
            .collect();
        filtered.sort_by(transcript_order);
        filtered.truncate(limit);
        Ok(filtered)
    }

    async fn latest_cursor(
        &self,
        channel: Option<RoomId>,
    ) -> Result<Option<TranscriptCursor>, StoreError> {
        let events = self.events.lock().map_err(|_| StoreError::LockPoisoned)?;
        let newest = events
            .iter()
            .filter(|e| channel.is_none_or(|room| e.room_id == room))
            .max_by(|a, b| transcript_order(a, b));
        Ok(newest.map(|e| e.cursor()))
    }

    async fn load_runtime_cursor(
        &self,
        consumer_id: &str,
    ) -> Result<Option<TranscriptCursor>, StoreError> {
        let cursors = self
            .runtime_cursors
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        Ok(cursors.get(consumer_id).cloned())
    }

    async fn save_runtime_cursor(
        &self,
        consumer_id: &str,
        cursor: &TranscriptCursor,
        _updated_at_ms: u64,
    ) -> Result<(), StoreError> {
        let mut cursors = self
            .runtime_cursors
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        cursors.insert(consumer_id.to_string(), cursor.clone());
        Ok(())
    }

    async fn load_subscriptions(&self) -> Result<Vec<StoredSubscription>, StoreError> {
        let subscriptions = self
            .subscriptions
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        Ok(subscriptions.clone())
    }

    async fn replace_subscriptions(&self, rows: Vec<StoredSubscription>) -> Result<(), StoreError> {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        *subscriptions = rows;
        Ok(())
    }
}

fn transcript_order(a: &TranscriptEvent, b: &TranscriptEvent) -> std::cmp::Ordering {
    a.lamport
        .cmp(&b.lamport)
        .then_with(|| a.event_id.as_uuid().cmp(&b.event_id.as_uuid()))
}

fn strictly_after(event: &TranscriptEvent, cursor: &TranscriptCursor) -> bool {
    if event.lamport > cursor.lamport {
        return true;
    }
    if event.lamport == cursor.lamport {
        return event.event_id.as_uuid() > cursor.event_id.as_uuid();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{
        body::Body,
        transcript::{MentionTarget, TranscriptKind},
        ClientId, EventId, Headers, PeerId, RoomId,
    };

    fn make_event(lamport: u64, room: RoomId, body: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: room,
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text(body)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    // Lighter-weight than the SQLite suite — the SqliteEventStore
    // tests exhaustively cover the contract. These confirm the
    // in-memory implementation is wire-compatible for any consumer
    // that swaps it in for tests.

    #[tokio::test]
    async fn in_memory_round_trips_one_event() {
        let store = InMemoryEventStore::new();
        let room = RoomId::from_u128(0xc0ffee);
        let ev = make_event(1, room, "hello");
        store.append(ev.clone()).await.unwrap();
        let page = store.page_recent(Some(room), 10).await.unwrap();
        assert_eq!(page, vec![ev]);
    }

    #[tokio::test]
    async fn in_memory_duplicate_event_id_errors() {
        let store = InMemoryEventStore::new();
        let room = RoomId::from_u128(0xc0ffee);
        let ev = make_event(1, room, "hi");
        store.append(ev.clone()).await.unwrap();
        let second = store.append(ev.clone()).await;
        assert!(matches!(second, Err(StoreError::DuplicateEventId(_))));
    }

    #[tokio::test]
    async fn in_memory_resume_from_skips_at_or_before_cursor() {
        let store = InMemoryEventStore::new();
        let room = RoomId::from_u128(0xc0ffee);
        let mut events = Vec::new();
        for i in 1..=4u64 {
            let ev = make_event(i, room, &format!("msg{i}"));
            events.push(ev.clone());
            store.append(ev).await.unwrap();
        }
        let after = store
            .resume_from(&events[1].cursor(), Some(room), 10)
            .await
            .unwrap();
        let lamports: Vec<u64> = after.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![3, 4]);
    }
}
