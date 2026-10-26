//! Live delivery with output-owned acknowledgment, and pre-attachment history
//! summarized once instead of replayed.
//!
//! A consumer's first turn (and a native feed's every attachment) finds history it
//! was never attached for: everything between its last settled position and the
//! present. That history used to be paged out as "retained catch-up" a few items per
//! turn, oldest first, so a scope with months of backlog emitted months-old chat on
//! every hook call and never drained (card ba22c72c, the M5 on 2026-10-26). It is now
//! counted once ([`AgentDeliveryTurn::history_summary`]); the caller shows that one
//! line, settles it on acknowledgment, and from then on carries only events that
//! arrived after attachment. The history itself stays durable for `airc inbox`.
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
/// What a turn found between a consumer's settled position and its attachment:
/// counted, not replayed (card ba22c72c).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySummary {
    /// Kept events counted in the range.
    pub count: usize,
    /// The count stopped at one scan page; the range holds more than `count`.
    pub at_least: bool,
    /// Original timestamps of the oldest and newest kept events.
    pub oldest_ms: u64,
    pub newest_ms: u64,
}
/// Events read from the newest end of a history range when the counting page
/// stopped short, to find the newest kept event. A bound on work, not a tuning
/// value: a range whose newest 64 events are all hidden reports the counted
/// page's newest instead, which is still an original timestamp from the range.
const HISTORY_TAIL: usize = 64;
/// Owns exclusive delivery rights until acknowledged or dropped. Output must be
/// flushed before `acknowledge`. Dropping without ack retains the live range and
/// the unsettled history. Crash after output but before ack may replay; stdout has
/// no transactional ack. Acknowledging marks positions DELIVERED to this consumer;
/// whether a reader consumed what it was shown is the reader's own fact.
pub struct AgentDeliveryTurn {
    airc: Airc,
    consumer: String,
    state: State,
    _guard: DeliveryLock,
    live: EventScan,
    through: TranscriptCursor,
    channels: Vec<RoomId>,
    scan_limit: usize,
    pub budget: usize,
}
impl AgentDeliveryTurn {
    pub fn live_events(&self) -> &[TranscriptEvent] {
        &self.live.events
    }
    pub fn retain(&mut self, filter: &EventFilter) {
        self.live.events.retain(|e| filter.matches(e));
    }
    pub fn is_empty(&self) -> bool {
        self.live.events.is_empty()
    }
    pub fn has_pending(&self, live_end: usize) -> bool {
        live_end < self.live.events.len()
            || self
                .live
                .scanned_through
                .as_ref()
                .is_some_and(|c| key(c) < key(&self.through))
    }
    /// True when history before attachment is still unsettled.
    pub fn has_history(&self) -> bool {
        key(&self.state.backlog) < key(&self.state.boundary)
    }
    /// Count the unsettled pre-attachment history that `keep` would show, with the
    /// original timestamps of its oldest and newest kept events. One forward page
    /// counts (exact up to the scan limit, `at_least` beyond it); `None` when there
    /// is no history or none of it is kept.
    pub async fn history_summary(
        &self,
        keep: impl Fn(&TranscriptEvent) -> bool,
    ) -> Result<Option<HistorySummary>, AircError> {
        if !self.has_history() {
            return Ok(None);
        }
        let (after, boundary) = (&self.state.backlog, &self.state.boundary);
        let page = self
            .airc
            .scan_agent_feed_in_channels(Some(after), boundary, self.scan_limit, &self.channels)
            .await?;
        let kept: Vec<_> = page.events.iter().filter(|e| keep(e)).collect();
        let at_least = page
            .scanned_through
            .as_ref()
            .is_some_and(|c| key(c) < key(boundary));
        let (Some(oldest), Some(last)) = (kept.first(), kept.last()) else {
            return Ok(None);
        };
        let mut newest_ms = last.occurred_at_ms;
        if at_least {
            let tail = self
                .airc
                .scan_agent_feed_in_channels(None, boundary, HISTORY_TAIL, &self.channels)
                .await?;
            if let Some(newest) = tail
                .events
                .iter()
                .rev()
                .find(|e| key(&e.cursor()) > key(after) && keep(e))
            {
                newest_ms = newest.occurred_at_ms;
            }
        }
        Ok(Some(HistorySummary {
            count: kept.len(),
            at_least,
            oldest_ms: oldest.occurred_at_ms,
            newest_ms,
        }))
    }
    /// Consumes the guard. `settle_history` records that the caller has shown the
    /// history summary (or that there was nothing in it worth showing), so the
    /// next turn starts at the present. The finite commit task retains OS
    /// ownership even if its caller is cancelled while the database driver still
    /// executes the write.
    pub async fn acknowledge(self, live_end: usize, settle_history: bool) -> Result<(), AircError> {
        tokio::spawn(async move {
            let mut turn = self;
            if rooms(&turn.airc).await? != turn.state.rooms {
                return Err(fail("subscription scope changed; checkpoint retained"));
            }
            let live = prefix(&turn.live, live_end, &turn.state.live)?;
            if key(&live) < key(&turn.state.live) || key(&live) > key(&turn.through) {
                return Err(fail("acknowledgment outside prepared range"));
            }
            turn.state.live = live;
            if settle_history {
                turn.state.backlog = turn.state.boundary.clone();
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
    /// Initialize the partition once, then prepare a bounded live page. History
    /// before the first attachment is left for [`AgentDeliveryTurn::history_summary`].
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
            airc.prepare_delivery_owned(consumer, scan_limit, budget, false)
                .await
        })
        .await
        .map_err(fail)?
    }
    /// Prepare an all-subscription native feed. On attachment, reclassify all currently
    /// unread events as pre-attachment history under the same exclusive delivery lock.
    /// This never acknowledges history or changes another consumer's checkpoint.
    pub async fn prepare_native_delivery(
        &self,
        consumer: &str,
        attaching: bool,
        scan_limit: usize,
        budget: usize,
    ) -> Result<AgentDeliveryTurn, AircError> {
        let airc = self.clone();
        let consumer = consumer.to_owned();
        tokio::spawn(async move {
            airc.prepare_delivery_owned(consumer, scan_limit, budget, attaching)
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
        attaching: bool,
    ) -> Result<AgentDeliveryTurn, AircError> {
        if !(1..=4096).contains(&scan_limit) || budget == 0 {
            return Err(fail("invalid scan/output budget"));
        }
        let guard = lock(self.home(), &consumer)?;
        let channels = rooms(self).await?;
        let source = if let Some(client) = self.daemon_client() {
            // The owner's stable identity identifies the router sequence authority;
            // embedded local Lamport values are a different cursor domain.
            // Typed, not flattened: a reader decides from it whether the daemon is gone.
            let owner = client.status().await?.peer_id;
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
        let mut state =
            if let Some(row) = self.inner.store.get_scoped_state(SCOPE, &consumer).await? {
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
                    rooms: channels.clone(),
                    source,
                };
                persist(self, &consumer, &state).await?;
                state
            };
        if attaching {
            // What arrived while detached becomes this attachment's history. A
            // settled consumer's history starts where its live delivery stopped,
            // so nothing already delivered is counted again; an unsettled one keeps
            // its oldest position until a summary of it is shown. History is never
            // replayed as live output; `airc inbox` remains the history reader.
            if key(&state.backlog) >= key(&state.boundary) {
                state.backlog = state.live.clone();
            }
            if key(&tip) > key(&state.live) {
                state.live = tip.clone();
            }
            state.boundary = state.live.clone();
            persist(self, &consumer, &state).await?;
        }
        let through = if key(&state.live) > key(&tip) {
            state.live.clone()
        } else {
            tip
        };
        let live = self
            .scan_agent_feed_in_channels(Some(&state.live), &through, scan_limit, &channels)
            .await?;
        if rooms(self).await? != channels {
            return Err(fail(
                "subscription scope changed during prepare; retry required",
            ));
        }
        Ok(AgentDeliveryTurn {
            airc: self.clone(),
            consumer,
            state,
            _guard: guard,
            live,
            through,
            channels,
            scan_limit,
            budget,
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
    // what this catches: a native reattach counts only what arrived while detached,
    // never re-counting what live delivery already showed, and keeps unsettled history.
    #[tokio::test]
    async fn native_attach_summarizes_only_the_detached_gap() {
        let (_dir, airc, room) = fixture().await;
        let first = airc
            .prepare_native_delivery("native", true, 64, 64)
            .await
            .unwrap();
        assert!(!first.has_history());
        first.acknowledge(0, true).await.unwrap();
        append(&airc, room, 1).await;
        let live = airc
            .prepare_native_delivery("native", false, 64, 64)
            .await
            .unwrap();
        assert_eq!(live.live_events()[0].lamport, 1);
        live.acknowledge(1, true).await.unwrap();
        append(&airc, room, 2).await;
        append(&airc, room, 3).await;
        let attached = airc
            .prepare_native_delivery("native", true, 64, 64)
            .await
            .unwrap();
        assert!(attached.live_events().is_empty());
        let summary = attached.history_summary(|_| true).await.unwrap().unwrap();
        assert_eq!(
            (summary.count, summary.oldest_ms, summary.newest_ms),
            (2, 2, 3)
        );
        drop(attached); // summary not shown: it stays unsettled across reattach
        append(&airc, room, 4).await;
        let again = airc
            .prepare_native_delivery("native", true, 64, 64)
            .await
            .unwrap();
        assert_eq!(
            again
                .history_summary(|_| true)
                .await
                .unwrap()
                .unwrap()
                .count,
            3
        );
        again.acknowledge(0, true).await.unwrap();
        append(&airc, room, 5).await;
        let fresh = airc
            .prepare_native_delivery("native", false, 64, 64)
            .await
            .unwrap();
        assert!(!fresh.has_history());
        assert_eq!(fresh.live_events()[0].lamport, 5);
    }
    // what this catches: card ba22c72c. A long backlog is one summary with its
    // original times, then only events newer than attachment; it never re-pages.
    #[tokio::test]
    async fn backlog_is_summarized_once_then_only_new_events_deliver() {
        let (dir, airc, room) = fixture().await;
        for n in 1..=1000 {
            append(&airc, room, n).await;
        }
        let initial = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(initial.live_events().is_empty());
        let summary = initial.history_summary(|_| true).await.unwrap().unwrap();
        assert_eq!(
            summary,
            HistorySummary {
                count: 64,
                at_least: true,
                oldest_ms: 1,
                newest_ms: 1000
            }
        );
        let hidden_newest = initial
            .history_summary(|e| e.lamport != 1000)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hidden_newest.newest_ms, 999);
        drop(initial); // output failed: the summary stays due
        append(&airc, room, 1001).await;
        let first = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(first.has_history());
        assert_eq!(first.live_events()[0].lamport, 1001);
        first.acknowledge(1, true).await.unwrap();
        assert!(airc
            .load_runtime_cursor("consumer")
            .await
            .unwrap()
            .is_none()); // legacy untouched
        let reopened = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        let settled = reopened
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(!settled.has_history());
        assert!(settled.history_summary(|_| true).await.unwrap().is_none());
        assert!(settled.is_empty());
        drop(settled);
        append(&reopened, room, 1002).await;
        let next = reopened
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert_eq!(next.live_events().len(), 1);
        assert_eq!(next.live_events()[0].lamport, 1002);
    }
    // what this catches: output that failed before acknowledgment replays (at least once).
    #[tokio::test]
    async fn unacknowledged_output_replays() {
        let (_dir, airc, room) = fixture().await;
        airc.prepare_agent_delivery("one", 8, 1)
            .await
            .unwrap()
            .acknowledge(0, true)
            .await
            .unwrap();
        append(&airc, room, 1).await;
        append(&airc, room, 2).await;
        let first = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
        assert!(first.has_pending(1));
        drop(first); // includes crash-after-output: at-least-once, not silent loss
        let replay = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
        assert_eq!(replay.live_events()[0].lamport, 1);
        replay.acknowledge(1, false).await.unwrap();
        let second = airc.prepare_agent_delivery("one", 8, 1).await.unwrap();
        assert_eq!(second.live_events()[0].lamport, 2);
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
