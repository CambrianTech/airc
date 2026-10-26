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
/// One consumer's persisted delivery state. Two watermarks with two meanings,
/// never one cursor for both:
/// - `live` is the DELIVERY proof: the last position whose output this consumer
///   flushed and acknowledged. Only `acknowledge(live_end, ..)` moves it.
/// - `summarized` is the SUMMARY watermark: history before attachment counted
///   and shown through here. Only `acknowledge(.., settle_history)` moves it (to
///   `boundary`). Settling a summary marks nothing delivered, and delivering
///   marks no history summarized.
///
/// `boundary` is the attachment point between the two ranges. `summarized` keeps
/// its original persisted key, `backlog`, so existing state rows still load.
#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u32,
    boundary: TranscriptCursor,
    #[serde(rename = "backlog")]
    summarized: TranscriptCursor,
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
/// What a turn found between a consumer's settled position and its attachment.
/// Room history is counted by the store, never replayed; a request addressed to
/// this identity is never reduced to the count (card ba22c72c, and the
/// DeliverySelection contract that an authorized direct request is never
/// filtered out).
#[derive(Clone, Debug, PartialEq)]
pub struct HistorySummary {
    /// Matching events in the range, with their oldest and newest times.
    pub counted: airc_core::HistoryCount,
    /// One reader page of the events addressed to this identity, newest first.
    pub addressed: Vec<TranscriptEvent>,
    /// Addressed events found beyond that page.
    pub more_addressed: usize,
    /// Whether the addressed search covered the whole range. Until card 6fb2ea16
    /// it reads only the newest candidates, so older requests may exist.
    pub addressed_complete: bool,
}
/// How a turn looks for addressed history: how many of the newest events it may
/// read (the interim, see [`AgentDeliveryTurn::addressed_history`]) and how many
/// it shows.
#[derive(Clone, Copy, Debug)]
pub struct AddressedWindow {
    pub candidates: usize,
    pub page: usize,
}
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
        key(&self.state.summarized) < key(&self.state.boundary)
    }
    /// Summarize the unsettled pre-attachment history in one turn: the store
    /// counts the events matching `count_filter` (headers only, no event read),
    /// and [`Self::addressed_history`] finds one page of the requests addressed
    /// to `me` that `keep` shows. `own_sends` is the ONE self-echo rule, applied
    /// to both: the store translates it for the count, the page applies it as is.
    /// `None` when there is neither. Acknowledging with `settle_history` then
    /// settles the whole range.
    pub async fn history_summary(
        &self,
        count_filter: airc_core::HeaderFilter,
        own_sends: Option<&airc_core::RuntimeSelf>,
        keep: impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        window: AddressedWindow,
    ) -> Result<Option<HistorySummary>, AircError> {
        if !self.has_history() {
            return Ok(None);
        }
        let counted = self
            .airc
            .count_agent_feed_in_channels(
                &self.state.summarized,
                &self.state.boundary,
                &self.channels,
                count_filter,
                own_sends,
            )
            .await?;
        let shown = |e: &TranscriptEvent| keep(e) && !own_sends.is_some_and(|r| r.is_self_event(e));
        let (addressed, more_addressed, addressed_complete) =
            self.addressed_history(shown, me, window).await?;
        if counted.count == 0 && addressed.is_empty() {
            return Ok(None);
        }
        Ok(Some(HistorySummary {
            counted,
            addressed,
            more_addressed,
            addressed_complete,
        }))
    }
    /// The requests addressed to `me` in the unsettled history, newest first:
    /// one page, how many more were found, and whether the search was complete.
    ///
    /// INTERIM until card 6fb2ea16 (Cormac): the store cannot yet select by
    /// addressee (the typed target is a JSON column and a leading `@name` lives
    /// in the body), so this reads only the newest `window.candidates` events of
    /// the range, one bounded tail scan, and applies [`crate::Addressee`] to
    /// them. When 6fb2ea16's indexed addressee query lands, this body becomes
    /// that query and the window's `candidates` goes away; callers do not change.
    pub async fn addressed_history(
        &self,
        keep: impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        window: AddressedWindow,
    ) -> Result<(Vec<TranscriptEvent>, usize, bool), AircError> {
        let (after, boundary) = (&self.state.summarized, &self.state.boundary);
        let tail = self
            .airc
            .scan_agent_feed_in_channels(None, boundary, window.candidates.max(1), &self.channels)
            .await?;
        let complete = tail
            .events
            .first()
            .is_none_or(|e| key(&e.cursor()) <= key(after))
            || tail.events.len() < window.candidates.max(1);
        let mut found: Vec<_> = tail
            .events
            .into_iter()
            .filter(|e| key(&e.cursor()) > key(after) && keep(e) && me.is_addressed(e))
            .collect();
        found.reverse();
        let page = window.page.max(1);
        let more = found.len().saturating_sub(page);
        found.truncate(page);
        Ok((found, more, complete))
    }
    /// Consumes the guard. `settle_history` records that the caller has shown what
    /// `history_summary` returned (or that it returned nothing), so the whole
    /// unsettled range is settled and the next turn starts at the present. The finite commit task retains OS
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
                turn.state.summarized = turn.state.boundary.clone();
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
                    || key(&state.summarized) > key(&state.boundary)
                    || key(&state.boundary) > key(&state.live)
                {
                    return Err(fail(
                        "unsupported or changed delivery scope/state; history retained",
                    ));
                }
                state
            } else {
                // A legacy checkpoint seeds the summary watermark: what it already
                // covered needs no summary.
                let summarized = self
                    .load_runtime_cursor(&consumer)
                    .await?
                    .unwrap_or_else(zero);
                let boundary = if key(&summarized) > key(&tip) {
                    summarized.clone()
                } else {
                    tip.clone()
                };
                let state = State {
                    version: 1,
                    live: boundary.clone(),
                    boundary,
                    summarized,
                    rooms: channels.clone(),
                    source,
                };
                persist(self, &consumer, &state).await?;
                state
            };
        if attaching {
            // What arrived while detached becomes this attachment's history. A
            // settled consumer's next summary starts where its live delivery
            // stopped (a one-way derivation: delivered needs no summary), so
            // nothing already delivered is counted again; an unsettled one keeps
            // its oldest position until a summary of it is shown. History is never
            // replayed as live output; `airc inbox` remains the history reader.
            if key(&state.summarized) >= key(&state.boundary) {
                state.summarized = state.live.clone();
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
    const WINDOW: AddressedWindow = AddressedWindow {
        candidates: 64,
        page: 8,
    };
    async fn summary(turn: &AgentDeliveryTurn, airc: &Airc) -> Option<HistorySummary> {
        let me = airc.addressee().await.unwrap();
        turn.history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, WINDOW)
            .await
            .unwrap()
    }
    fn counted(count: u64, oldest: u64, newest: u64) -> airc_core::HistoryCount {
        airc_core::HistoryCount {
            count,
            oldest_ms: Some(oldest),
            newest_ms: Some(newest),
        }
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
        assert_eq!(
            summary(&attached, &airc).await.unwrap().counted,
            counted(2, 2, 3)
        );
        drop(attached); // summary not shown: it stays unsettled across reattach
        append(&airc, room, 4).await;
        let again = airc
            .prepare_native_delivery("native", true, 64, 64)
            .await
            .unwrap();
        assert_eq!(summary(&again, &airc).await.unwrap().counted.count, 3);
        again.acknowledge(0, true).await.unwrap();
        append(&airc, room, 5).await;
        let fresh = airc
            .prepare_native_delivery("native", false, 64, 64)
            .await
            .unwrap();
        assert!(!fresh.has_history());
        assert_eq!(fresh.live_events()[0].lamport, 5);
    }
    // what this catches: card ba22c72c. A long backlog is counted by the store in ONE
    // turn with its original times (header-routed, no event read) and settles in that
    // turn; only events newer than attachment deliver afterwards, across a restart.
    #[tokio::test]
    async fn backlog_is_counted_once_and_settles_in_one_turn() {
        let (dir, airc, room) = fixture().await;
        for n in 1..=4999 {
            append(&airc, room, n).await;
        }
        airc.append_event(TranscriptEvent {
            event_id: EventId::from_u128(5000),
            room_id: room,
            peer_id: airc.peer_id(),
            client_id: airc.client_id(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 5000,
            lamport: 5000,
            target: MentionTarget::All,
            headers: Headers::from([("airc.test.hidden".to_string(), "1".to_string())]),
            body: Some(Body::text("hidden by its header")),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        })
        .await
        .unwrap();
        let initial = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(initial.live_events().is_empty());
        let found = summary(&initial, &airc).await.unwrap();
        assert_eq!(found.counted, counted(5000, 1, 5000));
        assert!(found.addressed.is_empty());
        let routed = initial
            .history_summary(
                airc_core::HeaderFilter::Not(Box::new(airc_core::HeaderFilter::Has {
                    key: "airc.test.hidden".into(),
                })),
                None,
                |_| true,
                &airc.addressee().await.unwrap(),
                WINDOW,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            routed.counted.count,
            5000 - 1,
            "header-routed out of the count"
        );
        drop(initial); // output failed: the summary stays due
        append(&airc, room, 5001).await;
        let first = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(first.has_history());
        assert_eq!(first.live_events()[0].lamport, 5001);
        assert!(summary(&first, &airc).await.is_some());
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
        assert!(!settled.has_history(), "settled in one turn, no drain");
        assert!(summary(&settled, &reopened).await.is_none());
        assert!(settled.is_empty());
        drop(settled);
        append(&reopened, room, 5002).await;
        let next = reopened
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert_eq!(next.live_events().len(), 1);
        assert_eq!(next.live_events()[0].lamport, 5002);
    }
    // what this catches: a request addressed to this identity is a page of events,
    // newest first, never folded into the count; the interim search reads only the
    // newest candidates and says when it was incomplete.
    #[tokio::test]
    async fn addressed_history_is_a_newest_first_page_from_the_newest_candidates() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=100 {
            append(&airc, room, n).await;
        }
        for n in 101..=120 {
            airc.append_event(TranscriptEvent {
                event_id: EventId::from_u128(n as u128),
                room_id: room,
                peer_id: airc.peer_id(),
                client_id: airc.client_id(),
                kind: TranscriptKind::Message,
                occurred_at_ms: n,
                lamport: n,
                target: MentionTarget::Peer(airc.peer_id()),
                headers: Headers::new(),
                body: Some(Body::text(format!("request-{n}"))),
                attachment: None,
                receipt: None,
                metadata: serde_json::Value::Null,
            })
            .await
            .unwrap();
        }
        let turn = airc.prepare_agent_delivery("paged", 64, 8).await.unwrap();
        let found = summary(&turn, &airc).await.unwrap();
        assert_eq!(found.counted.count, 120);
        let lamports: Vec<_> = found.addressed.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, (113..=120).rev().collect::<Vec<_>>());
        assert_eq!(found.more_addressed, 12);
        assert!(!found.addressed_complete, "only the newest 64 were read");
        turn.acknowledge(0, true).await.unwrap();
        let done = airc.prepare_agent_delivery("paged", 64, 8).await.unwrap();
        assert!(!done.has_history());
    }
    // what this catches (#1617 CI, red on 3 OSes): the history count and the
    // addressed page apply the SAME self-echo rule as live delivery, translated by
    // the store: an unstamped own send is left out, and a send stamped by another
    // runtime on this shared client still counts.
    #[tokio::test]
    async fn history_applies_the_one_self_echo_rule_in_the_store() {
        let (_dir, airc, room) = fixture().await;
        let send = |n: u64, stamp: Option<&str>, target: MentionTarget| TranscriptEvent {
            event_id: EventId::from_u128(n as u128),
            room_id: room,
            peer_id: airc.peer_id(),
            client_id: airc.client_id(),
            kind: TranscriptKind::Message,
            occurred_at_ms: n,
            lamport: n,
            target,
            headers: stamp
                .map(|tag| {
                    Headers::from([(
                        airc_core::headers::HEADER_AIRC_CLIENT.to_string(),
                        tag.to_string(),
                    )])
                })
                .unwrap_or_default(),
            body: Some(Body::text(format!("send-{n}"))),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        let me_peer = MentionTarget::Peer(airc.peer_id());
        airc.append_event(send(1, None, MentionTarget::All))
            .await
            .unwrap();
        airc.append_event(send(2, Some("codex:other"), MentionTarget::All))
            .await
            .unwrap();
        airc.append_event(send(3, None, me_peer.clone()))
            .await
            .unwrap();
        airc.append_event(send(4, Some("codex:other"), me_peer))
            .await
            .unwrap();
        let turn = airc
            .prepare_agent_delivery("self-rule", 64, 8)
            .await
            .unwrap();
        let me = airc.addressee().await.unwrap();
        let own = airc_core::RuntimeSelf::new(airc.client_id(), None);
        let found = turn
            .history_summary(
                airc_core::HeaderFilter::Any,
                Some(&own),
                |_| true,
                &me,
                WINDOW,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.counted.count, 2, "the two other-runtime sends");
        let lamports: Vec<_> = found.addressed.iter().map(|e| e.lamport).collect();
        assert_eq!(
            lamports,
            [4],
            "the own request is hidden, the other runtime's shown"
        );
        let everything = summary(&turn, &airc).await.unwrap();
        assert_eq!(everything.counted.count, 4);
    }
    // what this catches (Codex on #1617): the summary watermark and the delivery
    // proof are separate persisted fields. Settling a summary marks no live event
    // delivered, and delivering live events marks no history summarized.
    #[tokio::test]
    async fn settling_a_summary_and_delivering_live_move_separate_watermarks() {
        let (_dir, airc, room) = fixture().await;
        append(&airc, room, 1).await;
        let first = airc.prepare_agent_delivery("marks", 64, 8).await.unwrap();
        let (summarized, live) = (first.state.summarized.clone(), first.state.live.clone());
        assert!(first.has_history());
        drop(first);
        append(&airc, room, 2).await;
        // Settle the summary but deliver nothing: live 2 stays undelivered.
        let settle = airc.prepare_agent_delivery("marks", 64, 8).await.unwrap();
        assert_eq!(settle.live_events()[0].lamport, 2);
        settle.acknowledge(0, true).await.unwrap();
        let after_settle = airc.prepare_agent_delivery("marks", 64, 8).await.unwrap();
        assert!(!after_settle.has_history());
        assert_eq!(
            after_settle.state.live.lamport, live.lamport,
            "nothing delivered"
        );
        assert_eq!(after_settle.live_events()[0].lamport, 2, "live 2 still due");
        assert!(key(&after_settle.state.summarized) > key(&summarized));
        drop(after_settle);
        // A fresh consumer delivers live without settling: history stays due.
        let other = airc.prepare_agent_delivery("marks-2", 64, 8).await.unwrap();
        let summarized = other.state.summarized.clone();
        drop(other);
        append(&airc, room, 3).await;
        let deliver = airc.prepare_agent_delivery("marks-2", 64, 8).await.unwrap();
        assert_eq!(deliver.live_events()[0].lamport, 3);
        deliver.acknowledge(1, false).await.unwrap();
        let after_deliver = airc.prepare_agent_delivery("marks-2", 64, 8).await.unwrap();
        assert!(after_deliver.has_history(), "history still unsummarized");
        assert_eq!(after_deliver.state.summarized.lamport, summarized.lamport);
        assert_eq!(after_deliver.state.live.lamport, 3, "delivered");
        // The persisted key is unchanged, so rows written before the rename load.
        let row = airc
            .inner
            .store
            .get_scoped_state(SCOPE, "marks-2")
            .await
            .unwrap()
            .unwrap();
        assert!(row.value_json.contains("\"backlog\""));
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
