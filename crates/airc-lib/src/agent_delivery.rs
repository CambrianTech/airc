//! Independent live delivery and retained catch-up, with output-owned acknowledgment.
use crate::{Airc, AircError, EventFilter, EventScan};
use airc_core::{EventId, RoomId, TranscriptCursor, TranscriptEvent};
use airc_store::StoredScopedState;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    path::Path,
};
const SCOPE: &str = "runtime:agent-delivery";
fn fail(message: impl std::fmt::Display) -> AircError {
    AircError::Route(format!("agent delivery: {message}"))
}
fn key(c: &TranscriptCursor) -> (u64, u128) {
    (c.lamport, c.event_id.0.as_u128())
}
fn zero() -> TranscriptCursor {
    TranscriptCursor {
        lamport: 0,
        event_id: EventId::from_u128(0),
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u32,
    boundary: TranscriptCursor,
    backlog: TranscriptCursor,
    live: TranscriptCursor,
    next_live: bool,
    rooms: Vec<RoomId>,
    source: String,
}
struct DeliveryLock(File);
impl Drop for DeliveryLock {
    fn drop(&mut self) {
        // Closing one descriptor alone does not release a Unix flock while a
        // duplicated descriptor (including one inherited across fork) survives.
        // Release ownership explicitly when this delivery turn ends.
        if let Err(error) = fs2::FileExt::unlock(&self.0) {
            eprintln!("airc: delivery lock release failed: {error}");
        }
    }
}
fn lock(home: &Path, consumer: &str) -> Result<DeliveryLock, AircError> {
    // Resolve the actual database file, including directory/file symlinks and
    // relative aliases. A scope sharing this file therefore shares this lock.
    let db = home.join("events.sqlite").canonicalize().map_err(fail)?;
    let name = db
        .file_name()
        .ok_or_else(|| fail("database has no filename"))?
        .to_string_lossy();
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, consumer.as_bytes());
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(db.with_file_name(format!("{name}.agent-delivery-{id}.lock")))
        .map_err(fail)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|e| fail(format!("consumer busy or lock unavailable: {e}")))?;
    Ok(DeliveryLock(file)) // never unlink: process-held OS ownership, no TTL takeover
}
async fn rooms(airc: &Airc) -> Result<Vec<RoomId>, AircError> {
    let mut rooms: Vec<_> = airc
        .subscription_set()
        .await?
        .all()
        .map(|s| s.as_room().channel)
        .collect();
    rooms.sort_by_key(|r| r.0);
    rooms.dedup();
    Ok(rooms)
}
async fn persist(airc: &Airc, consumer: &str, state: &State) -> Result<(), AircError> {
    airc.inner
        .store
        .set_scoped_state(StoredScopedState {
            scope_key: SCOPE.into(),
            key: consumer.into(),
            value_json: serde_json::to_string(state).map_err(fail)?,
            version: 1,
            updated_at_ms: 0,
            updated_by: None,
        })
        .await?;
    Ok(())
}
/// Owns exclusive delivery rights until acknowledged or dropped. Output must be
/// flushed before `acknowledge`. Dropping without ack retains both unread ranges.
/// Crash after output but before ack may replay; stdout has no transactional ack.
pub struct AgentDeliveryTurn {
    airc: Airc,
    consumer: String,
    state: State,
    _guard: DeliveryLock,
    live: EventScan,
    backlog: EventScan,
    through: TranscriptCursor,
    pub live_budget: usize,
    pub backlog_budget: usize,
    single: bool,
}
impl AgentDeliveryTurn {
    pub fn live_events(&self) -> &[TranscriptEvent] {
        &self.live.events
    }
    pub fn backlog_events(&self) -> &[TranscriptEvent] {
        &self.backlog.events
    }
    pub fn retain(&mut self, filter: &EventFilter) {
        self.live.events.retain(|e| filter.matches(e));
        self.backlog.events.retain(|e| filter.matches(e));
    }
    pub fn is_empty(&self) -> bool {
        self.live.events.is_empty() && self.backlog.events.is_empty()
    }
    pub fn has_pending(&self, live_end: usize, backlog_end: usize) -> bool {
        live_end < self.live.events.len()
            || backlog_end < self.backlog.events.len()
            || self
                .live
                .scanned_through
                .as_ref()
                .is_some_and(|c| key(c) < key(&self.through))
            || self
                .backlog
                .scanned_through
                .as_ref()
                .is_some_and(|c| key(c) < key(&self.state.boundary))
    }
    /// Consumes the guard. The finite commit task retains OS ownership even if
    /// its caller is cancelled while the database driver still executes the write.
    pub async fn acknowledge(self, live_end: usize, backlog_end: usize) -> Result<(), AircError> {
        tokio::spawn(async move {
            let mut turn = self;
            if rooms(&turn.airc).await? != turn.state.rooms {
                return Err(fail("subscription scope changed; checkpoint retained"));
            }
            let live = prefix(&turn.live, live_end, &turn.state.live)?;
            let backlog = prefix(&turn.backlog, backlog_end, &turn.state.backlog)?;
            if key(&live) < key(&turn.state.live)
                || key(&live) > key(&turn.through)
                || key(&backlog) < key(&turn.state.backlog)
                || key(&backlog) > key(&turn.state.boundary)
            {
                return Err(fail("acknowledgment outside prepared range"));
            }
            turn.state.live = live;
            turn.state.backlog = backlog;
            if turn.single {
                turn.state.next_live = !turn.state.next_live;
            }
            persist(&turn.airc, &turn.consumer, &turn.state).await
        })
        .await
        .map_err(fail)?
    }
}
fn prefix(
    page: &EventScan,
    end: usize,
    previous: &TranscriptCursor,
) -> Result<TranscriptCursor, AircError> {
    if end > page.events.len() {
        return Err(fail("invalid delivered prefix"));
    }
    Ok(if end == page.events.len() {
        page.scanned_through
            .clone()
            .unwrap_or_else(|| previous.clone())
    } else if end > 0 {
        page.events[end - 1].cursor()
    } else {
        previous.clone()
    })
}
impl Airc {
    /// Initialize the partition once, then prepare bounded live and backlog pages.
    /// Scope changes fail explicitly: a global cursor cannot safely cover newly
    /// joined rooms' older events. Legacy checkpoints and general history are untouched.
    pub async fn prepare_agent_delivery(
        &self,
        consumer: &str,
        scan_limit: usize,
        budget: usize,
    ) -> Result<AgentDeliveryTurn, AircError> {
        let airc = self.clone();
        let consumer = consumer.to_owned();
        tokio::spawn(async move {
            airc.prepare_delivery_owned(consumer, scan_limit, budget)
                .await
        })
        .await
        .map_err(fail)?
    }
    async fn prepare_delivery_owned(
        &self,
        consumer: String,
        scan_limit: usize,
        budget: usize,
    ) -> Result<AgentDeliveryTurn, AircError> {
        if !(1..=4096).contains(&scan_limit) || budget == 0 {
            return Err(fail("invalid scan/output budget"));
        }
        let guard = lock(self.home(), &consumer)?;
        let channels = rooms(self).await?;
        let source = if let Some(client) = self.daemon_client() {
            // The owner's stable identity identifies the router sequence authority;
            // embedded local Lamport values are a different cursor domain.
            let owner = client.status().await.map_err(fail)?.peer_id;
            format!(
                "daemon-router:{}:{}",
                owner,
                self.wire_root().canonicalize().map_err(fail)?.display()
            )
        } else {
            format!(
                "embedded-transcript:{}",
                self.home()
                    .join("events.sqlite")
                    .canonicalize()
                    .map_err(fail)?
                    .display()
            )
        };
        let mut tip = zero();
        for channel in &channels {
            if let Some(cursor) = self.channel_latest_cursor(*channel).await? {
                if key(&cursor) > key(&tip) {
                    tip = cursor;
                }
            }
        }
        let state = if let Some(row) = self.inner.store.get_scoped_state(SCOPE, &consumer).await? {
            let state: State = serde_json::from_str(&row.value_json).map_err(fail)?;
            if row.version != 1
                || state.version != 1
                || state.rooms != channels
                || state.source != source
                || key(&state.backlog) > key(&state.boundary)
                || key(&state.boundary) > key(&state.live)
            {
                return Err(fail(
                    "unsupported or changed delivery scope/state; history retained",
                ));
            }
            state
        } else {
            let backlog = self
                .load_runtime_cursor(&consumer)
                .await?
                .unwrap_or_else(zero);
            let boundary = if key(&backlog) > key(&tip) {
                backlog.clone()
            } else {
                tip.clone()
            };
            let state = State {
                version: 1,
                live: boundary.clone(),
                boundary,
                backlog,
                next_live: true,
                rooms: channels.clone(),
                source,
            };
            persist(self, &consumer, &state).await?;
            state
        };
        let through = if key(&state.live) > key(&tip) {
            state.live.clone()
        } else {
            tip
        };
        let live = self
            .scan_agent_feed_in_channels(Some(&state.live), &through, scan_limit, &channels)
            .await?;
        let backlog = self
            .scan_agent_feed_in_channels(
                Some(&state.backlog),
                &state.boundary,
                scan_limit,
                &channels,
            )
            .await?;
        if rooms(self).await? != channels {
            return Err(fail(
                "subscription scope changed during prepare; retry required",
            ));
        }
        let (mut live_budget, mut backlog_budget) = if budget == 1 {
            if state.next_live {
                (1, 0)
            } else {
                (0, 1)
            }
        } else {
            (budget - 1, 1)
        };
        if live.events.is_empty() {
            backlog_budget = budget;
        }
        if backlog.events.is_empty() {
            live_budget = budget;
        }
        Ok(AgentDeliveryTurn {
            airc: self.clone(),
            consumer,
            state,
            _guard: guard,
            live,
            backlog,
            through,
            live_budget,
            backlog_budget,
            single: budget == 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscriptions::{self, ChannelName, MeshIdentity, SubscriptionSet};
    use airc_core::{Body, Headers, MentionTarget, TranscriptKind};
    async fn fixture() -> (tempfile::TempDir, Airc, RoomId) {
        let dir = tempfile::TempDir::new().unwrap();
        let airc = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        let mut set = SubscriptionSet::empty();
        let name = ChannelName::new("delivery-test").unwrap();
        let room = set
            .subscribe_with_wire_root(dir.path(), &MeshIdentity::new("test"), name.clone())
            .unwrap()
            .room_id;
        set.set_default(name).unwrap();
        subscriptions::save(airc.coordinator_store_for_test(), &set)
            .await
            .unwrap();
        (dir, airc, room)
    }
    async fn append(airc: &Airc, room: RoomId, n: u64) {
        airc.append_event(TranscriptEvent {
            event_id: EventId::from_u128(n as u128),
            room_id: room,
            peer_id: airc.peer_id(),
            client_id: airc.client_id(),
            kind: TranscriptKind::Message,
            occurred_at_ms: n,
            lamport: n,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text(format!("event-{n}"))),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn fresh_delivery_precedes_large_backlog_and_restart_recovers_every_prefix() {
        let (dir, airc, room) = fixture().await;
        for n in 1..=1000 {
            append(&airc, room, n).await;
        }
        let initial = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert_eq!(initial.state.boundary.lamport, 1000);
        drop(initial); // output failed/cancelled: no prefix acknowledged
        append(&airc, room, 1001).await;
        let first = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert_eq!(first.live_events()[0].lamport, 1001);
        assert_eq!(first.backlog_events()[0].lamport, 1);
        assert_eq!(first.backlog_budget, 1);
        first.acknowledge(1, 1).await.unwrap();
        assert!(airc
            .load_runtime_cursor("consumer")
            .await
            .unwrap()
            .is_none()); // legacy untouched
        let reopened = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        let mut seen = vec![1];
        loop {
            let turn = reopened
                .prepare_agent_delivery("consumer", 64, 64)
                .await
                .unwrap();
            assert!(turn.live_events().is_empty());
            assert_eq!(turn.state.boundary.lamport, 1000);
            let end = turn.backlog_events().len().min(turn.backlog_budget);
            seen.extend(turn.backlog_events()[..end].iter().map(|e| e.lamport));
            let pending = turn.has_pending(0, end);
            turn.acknowledge(0, end).await.unwrap();
            if !pending {
                break;
            }
        }
        assert_eq!(seen, (1..=1000).collect::<Vec<_>>());
    }
    #[tokio::test]
    async fn one_item_budget_alternates_and_unacknowledged_output_replays() {
        let (_dir, airc, room) = fixture().await;
        append(&airc, room, 1).await;
        drop(airc.prepare_agent_delivery("one", 8, 1).await.unwrap());
        append(&airc, room, 2).await;
        let first = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
        assert_eq!((first.live_budget, first.backlog_budget), (1, 0));
        drop(first); // includes crash-after-output: at-least-once, not silent loss
        let replay = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
        assert_eq!(replay.live_events()[0].lamport, 2);
        replay.acknowledge(1, 0).await.unwrap();
        append(&airc, room, 3).await;
        let second = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
        assert_eq!((second.live_budget, second.backlog_budget), (0, 1));
        assert_eq!(second.backlog_events()[0].lamport, 1);
        second.acknowledge(0, 1).await.unwrap();
        let third = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
        assert_eq!(third.live_events()[0].lamport, 3);
    }
    #[tokio::test]
    async fn aliases_and_concurrent_prepares_share_ownership_and_new_scope_refuses() {
        let (dir, airc, room) = fixture().await;
        append(&airc, room, 1).await;
        let turn = airc.prepare_agent_delivery("same", 8, 8).await.unwrap();
        assert!(lock(&dir.path().join("."), "same").is_err());
        assert!(airc.prepare_agent_delivery("same", 8, 8).await.is_err());
        assert!(lock(dir.path(), "different").is_ok());
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
            assert!(lock(&alias, "same").is_err());
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_delivery::tests::lock_child",
                "--nocapture",
            ])
            .env("AIRC_DELIVERY_LOCK_TEST_HOME", dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        drop(turn);
        assert!(lock(dir.path(), "same").is_ok());
        let mut set = airc.subscription_set().await.unwrap();
        set.subscribe_with_wire_root(
            dir.path(),
            &MeshIdentity::new("test"),
            ChannelName::new("added-room").unwrap(),
        )
        .unwrap();
        subscriptions::save(airc.coordinator_store_for_test(), &set)
            .await
            .unwrap();
        assert!(airc.prepare_agent_delivery("same", 8, 8).await.is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_turn_releases_lock_even_with_a_duplicated_descriptor() {
        let (dir, airc, _) = fixture().await;
        let turn = airc
            .prepare_agent_delivery("duplicated", 8, 8)
            .await
            .unwrap();
        let duplicate = turn._guard.0.try_clone().unwrap();
        assert!(lock(dir.path(), "duplicated").is_err());
        drop(turn);
        // The duplicate remains open: close-only ownership fails this assertion.
        let next = lock(dir.path(), "duplicated").unwrap();
        drop(duplicate);
        assert!(lock(dir.path(), "duplicated").is_err());
        drop(next);
        assert!(lock(dir.path(), "duplicated").is_ok());
    }
    #[test]
    fn lock_child() {
        if let Some(home) = std::env::var_os("AIRC_DELIVERY_LOCK_TEST_HOME") {
            assert!(lock(Path::new(&home), "same").is_err());
        }
    }
    #[tokio::test]
    async fn unknown_state_version_refuses_without_overwriting() {
        let (_dir, airc, _) = fixture().await;
        let turn = airc.prepare_agent_delivery("future", 8, 8).await.unwrap();
        let mut state = turn.state.clone();
        state.version = 99;
        drop(turn);
        persist(&airc, "future", &state).await.unwrap();
        assert!(airc.prepare_agent_delivery("future", 8, 8).await.is_err());
        let row = airc
            .inner
            .store
            .get_scoped_state(SCOPE, "future")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<State>(&row.value_json)
                .unwrap()
                .version,
            99
        );
    }
}
