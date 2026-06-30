//! In-memory `EventStore` — useful for tests and for tooling that
//! wants the trait shape without touching disk. Not durable; loses
//! all state when dropped.

use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Mutex;

use airc_core::{PeerId, RoomId, TranscriptCursor, TranscriptEvent};
use uuid::Uuid;

use crate::beacon::StoredBeacon;
use crate::error::StoreError;
use crate::local_identity::StoredLocalIdentity;
use crate::mesh_identity::StoredMeshIdentity;
use crate::refresh_lock::{StoredRefreshLock, StoredRefreshLockOutcome};
use crate::scoped_state::StoredScopedState;
use crate::store::EventStore;
use crate::subscriptions::StoredSubscription;

pub struct InMemoryEventStore {
    local_identities: Mutex<BTreeMap<String, StoredLocalIdentity>>,
    events: Mutex<Vec<TranscriptEvent>>,
    runtime_cursors: Mutex<BTreeMap<String, TranscriptCursor>>,
    subscriptions: Mutex<Vec<StoredSubscription>>,
    mesh_identities: Mutex<BTreeMap<String, StoredMeshIdentity>>,
    beacons: Mutex<BTreeMap<(String, Uuid), StoredBeacon>>,
    refresh_locks: Mutex<BTreeMap<String, StoredRefreshLock>>,
    scoped_state: Mutex<BTreeMap<(String, String), StoredScopedState>>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self {
            local_identities: Mutex::new(BTreeMap::new()),
            events: Mutex::new(Vec::new()),
            runtime_cursors: Mutex::new(BTreeMap::new()),
            subscriptions: Mutex::new(Vec::new()),
            mesh_identities: Mutex::new(BTreeMap::new()),
            beacons: Mutex::new(BTreeMap::new()),
            refresh_locks: Mutex::new(BTreeMap::new()),
            scoped_state: Mutex::new(BTreeMap::new()),
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
    async fn load_local_identity(&self) -> Result<Option<StoredLocalIdentity>, StoreError> {
        self.load_local_identity_by_agent_name(crate::DEFAULT_AGENT_NAME)
            .await
    }

    async fn load_local_identity_by_agent_name(
        &self,
        agent_name: &str,
    ) -> Result<Option<StoredLocalIdentity>, StoreError> {
        let identities = self
            .local_identities
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        Ok(identities.get(agent_name).cloned())
    }

    async fn insert_local_identity(&self, identity: StoredLocalIdentity) -> Result<(), StoreError> {
        let mut stored = self
            .local_identities
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        if stored.contains_key(&identity.agent_name) {
            return Err(StoreError::Database(sea_orm::DbErr::RecordNotInserted));
        }
        stored.insert(identity.agent_name.clone(), identity);
        Ok(())
    }

    async fn save_local_identity_card(
        &self,
        identity: airc_core::identity::Identity,
    ) -> Result<(), StoreError> {
        let mut stored = self
            .local_identities
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        let Some(row) = stored.get_mut(crate::DEFAULT_AGENT_NAME) else {
            return Err(StoreError::NotFound("local_identity"));
        };
        row.identity = identity;
        Ok(())
    }

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

    async fn load_mesh_identity(
        &self,
        scope: &str,
    ) -> Result<Option<StoredMeshIdentity>, StoreError> {
        let identities = self
            .mesh_identities
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        Ok(identities.get(scope).cloned())
    }

    async fn save_mesh_identity(&self, entry: StoredMeshIdentity) -> Result<(), StoreError> {
        let mut identities = self
            .mesh_identities
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        identities.insert(entry.scope.clone(), entry);
        Ok(())
    }

    async fn get_scoped_state(
        &self,
        scope_key: &str,
        key: &str,
    ) -> Result<Option<StoredScopedState>, StoreError> {
        let state = self
            .scoped_state
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        Ok(state
            .get(&(scope_key.to_string(), key.to_string()))
            .cloned())
    }

    async fn set_scoped_state(&self, entry: StoredScopedState) -> Result<(), StoreError> {
        let mut state = self
            .scoped_state
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        state.insert((entry.scope_key.clone(), entry.key.clone()), entry);
        Ok(())
    }

    async fn list_scoped_state(
        &self,
        scope_key: &str,
    ) -> Result<Vec<StoredScopedState>, StoreError> {
        let state = self
            .scoped_state
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        // BTreeMap iterates in (scope_key, key) order, so filtering by
        // scope_key yields the rows already key-ascending — matching the
        // SQLite `order_by_asc(Key)` contract.
        Ok(state
            .iter()
            .filter(|((sk, _), _)| sk == scope_key)
            .map(|(_, v)| v.clone())
            .collect())
    }

    async fn delete_scoped_state(&self, scope_key: &str, key: &str) -> Result<(), StoreError> {
        let mut state = self
            .scoped_state
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        state.remove(&(scope_key.to_string(), key.to_string()));
        Ok(())
    }

    async fn load_beacon(
        &self,
        mesh_identity: &str,
        peer_id: PeerId,
    ) -> Result<Option<StoredBeacon>, StoreError> {
        let beacons = self.beacons.lock().map_err(|_| StoreError::LockPoisoned)?;
        Ok(beacons
            .get(&(mesh_identity.to_string(), peer_id.as_uuid()))
            .cloned())
    }

    async fn list_beacons(&self, mesh_identity: &str) -> Result<Vec<StoredBeacon>, StoreError> {
        let beacons = self.beacons.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut rows = beacons
            .values()
            .filter(|beacon| beacon.mesh_identity == mesh_identity)
            .cloned()
            .collect::<Vec<_>>();
        rows.sort_by_key(|beacon| beacon.peer_id.to_string());
        Ok(rows)
    }

    async fn save_beacon(&self, beacon: StoredBeacon) -> Result<(), StoreError> {
        let mut beacons = self.beacons.lock().map_err(|_| StoreError::LockPoisoned)?;
        beacons.insert(
            (beacon.mesh_identity.clone(), beacon.peer_id.as_uuid()),
            beacon,
        );
        Ok(())
    }

    async fn delete_beacons(
        &self,
        mesh_identity: &str,
        peer_ids: &[PeerId],
    ) -> Result<usize, StoreError> {
        let mut beacons = self.beacons.lock().map_err(|_| StoreError::LockPoisoned)?;
        let mut removed = 0;
        for peer_id in peer_ids {
            if beacons
                .remove(&(mesh_identity.to_string(), peer_id.as_uuid()))
                .is_some()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn try_acquire_refresh_lock(
        &self,
        mesh_identity: &str,
        now_ms: u64,
        refresh_interval_ms: u64,
        holder_pid: u32,
    ) -> Result<StoredRefreshLockOutcome, StoreError> {
        let mut locks = self
            .refresh_locks
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        if let Some(existing) = locks.get(mesh_identity) {
            if now_ms.saturating_sub(existing.held_at_ms) < refresh_interval_ms {
                return Ok(StoredRefreshLockOutcome::HeldFresh {
                    held_at_ms: existing.held_at_ms,
                });
            }
        }
        locks.insert(
            mesh_identity.to_string(),
            StoredRefreshLock {
                mesh_identity: mesh_identity.to_string(),
                held_at_ms: now_ms,
                holder_pid,
            },
        );
        Ok(StoredRefreshLockOutcome::Acquired)
    }

    async fn release_refresh_lock(&self, mesh_identity: &str) -> Result<(), StoreError> {
        let mut locks = self
            .refresh_locks
            .lock()
            .map_err(|_| StoreError::LockPoisoned)?;
        locks.remove(mesh_identity);
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
    async fn in_memory_local_identity_card_matches_store_trait() {
        let store = InMemoryEventStore::new();
        let store_api: &dyn EventStore = &store;
        let mut identity = airc_core::identity::Identity::new("alice");
        identity.role = "tester".into();

        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xa1),
                client_id: ClientId::from_u128(0xc1),
                version: 1,
                created_at_ms: 42,
                identity,
                agent_name: crate::DEFAULT_AGENT_NAME.to_string(),
            })
            .await
            .unwrap();

        let mut updated = airc_core::identity::Identity::new("alice");
        updated.status = "green".into();
        store_api
            .save_local_identity_card(updated.clone())
            .await
            .unwrap();

        let stored = store_api.load_local_identity().await.unwrap().unwrap();
        assert_eq!(stored.identity, updated);
    }

    #[tokio::test]
    async fn in_memory_load_local_identity_is_default_agent_path() {
        let store = InMemoryEventStore::new();
        let store_api: &dyn EventStore = &store;

        store_api
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::from_u128(0xa2),
                client_id: airc_core::ClientId::from_u128(0xc2),
                version: 1,
                created_at_ms: 43,
                identity: airc_core::identity::Identity::new("codex"),
                agent_name: "codex".to_string(),
            })
            .await
            .unwrap();

        assert!(store_api.load_local_identity().await.unwrap().is_none());
        let by_name = store_api
            .load_local_identity_by_agent_name("codex")
            .await
            .unwrap()
            .expect("codex row");
        assert_eq!(by_name.agent_name, "codex");
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
