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
/// One consumer's persisted delivery state. Each position has ONE meaning:
/// - `live`: the DELIVERY proof, the end of the live range this consumer flushed
///   and acknowledged. Only `acknowledge(live_end, ..)` moves it, never an
///   attach: a reattach that is dropped before output leaves it where it was.
/// - `boundary`: where the current attachment started reading live. Events at or
///   before it are history; the live scan starts at `max(live, boundary)`.
/// - `summarized`: history settled through here (counted, and every addressed
///   request in it emitted). Moves only when a summary turn finishes the range.
/// - `history_counted`: the ambient count of `(summarized, boundary]` was shown.
/// - `addressed_below`: addressed requests at or after this position (and up to
///   `boundary`) have been emitted; the next page reads strictly below it.
///   `None`: none emitted yet in this range.
///
/// Settling a summary marks nothing delivered, delivering marks no history
/// summarized, and no settlement buries an addressed request not yet emitted.
/// `summarized` keeps its original persisted key, `backlog`, so existing rows load.
#[derive(Clone, Serialize, Deserialize)]
struct State {
    version: u32,
    boundary: TranscriptCursor,
    #[serde(rename = "backlog")]
    summarized: TranscriptCursor,
    live: TranscriptCursor,
    rooms: Vec<RoomId>,
    source: String,
    #[serde(default)]
    history_counted: bool,
    #[serde(default)]
    addressed_below: Option<TranscriptCursor>,
}
impl State {
    /// Where live delivery reads from: after both what was delivered and where
    /// this attachment began.
    fn scan_from(&self) -> &TranscriptCursor {
        if key(&self.live) > key(&self.boundary) {
            &self.live
        } else {
            &self.boundary
        }
    }
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
    /// Matching events in the range, with their oldest and newest times; shown
    /// once per range (`None` on later turns that only page addressed requests).
    pub counted: Option<airc_core::HistoryCount>,
    /// One reader page of the events addressed to this identity, newest first.
    pub addressed: Vec<TranscriptEvent>,
    /// Addressed events already found beyond that page; later turns emit them.
    pub more_addressed: usize,
    /// Whether this page finishes the range: no addressed request is left
    /// unshown. While false, the history stays unsettled and later turns page on.
    pub addressed_complete: bool,
}
struct AddressedPage {
    shown: Vec<TranscriptEvent>,
    more: usize,
    progress: AddressedProgress,
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
    /// What `history_summary` produced, applied by `acknowledge(.., true)`.
    progress: Option<HistoryProgress>,
    pub budget: usize,
}
/// How far one summary turn got: whether it showed the count, and how far down
/// the addressed requests are emitted (or that the range is finished).
#[derive(Clone)]
struct HistoryProgress {
    counted_shown: bool,
    addressed: AddressedProgress,
}
#[derive(Clone)]
enum AddressedProgress {
    /// Every addressed request in the range is emitted: the range settles.
    Finished,
    /// Addressed requests at or after this position are emitted (or searched).
    Below(TranscriptCursor),
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
    /// One turn of the unsettled pre-attachment history. The ambient count is
    /// the store's (headers only, no event read) and is shown once per range;
    /// addressed requests are paged newest first by [`Self::addressed_history`]
    /// until every one is emitted, and only then does the range settle.
    /// `own_sends` is the ONE self-echo rule, applied to the count (translated by
    /// the store) and to the page. `None` when this turn has nothing to show;
    /// `acknowledge(.., true)` records the turn's progress either way.
    pub async fn history_summary(
        &mut self,
        count_filter: airc_core::HeaderFilter,
        own_sends: Option<&airc_core::RuntimeSelf>,
        keep: impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        window: AddressedWindow,
    ) -> Result<Option<HistorySummary>, AircError> {
        self.progress = None;
        if !self.has_history() {
            return Ok(None);
        }
        let counted = if self.state.history_counted {
            None
        } else {
            let count = self
                .airc
                .count_agent_feed_in_channels(
                    &self.state.summarized,
                    &self.state.boundary,
                    &self.channels,
                    count_filter,
                    own_sends,
                )
                .await?;
            (count.count > 0).then_some(count)
        };
        let shown = |e: &TranscriptEvent| keep(e) && !own_sends.is_some_and(|r| r.is_self_event(e));
        let page = self.addressed_history(shown, me, window).await?;
        let addressed_complete = matches!(page.progress, AddressedProgress::Finished);
        self.progress = Some(HistoryProgress {
            counted_shown: !self.state.history_counted,
            addressed: page.progress,
        });
        if counted.is_none() && page.shown.is_empty() {
            return Ok(None);
        }
        Ok(Some(HistorySummary {
            counted,
            addressed: page.shown,
            more_addressed: page.more,
            addressed_complete,
        }))
    }
    /// The next page of requests addressed to `me` in the unsettled history,
    /// newest first, strictly below what earlier turns emitted.
    ///
    /// INTERIM until card 6fb2ea16 (Cormac): the store cannot yet select by
    /// addressee (the typed target is a JSON column and a leading `@name` lives
    /// in the body), so one turn reads only the newest `window.candidates`
    /// events below the checkpoint, one bounded tail scan, and applies
    /// [`crate::Addressee`] to them. The checkpoint then moves down only through
    /// what was actually emitted or searched, so a request the bounded search
    /// has not reached is never settled past. When 6fb2ea16's indexed addressee
    /// query lands, this body becomes that query and callers do not change.
    async fn addressed_history(
        &self,
        keep: impl Fn(&TranscriptEvent) -> bool,
        me: &crate::Addressee,
        window: AddressedWindow,
    ) -> Result<AddressedPage, AircError> {
        let after = &self.state.summarized;
        let candidates = window.candidates.max(1);
        // Inclusive upper bound for the scan; an exclusive checkpoint reads one
        // more and drops the checkpoint event itself.
        let (upper, exclusive) = match &self.state.addressed_below {
            Some(below) => (below, true),
            None => (&self.state.boundary, false),
        };
        let limit = (candidates + usize::from(exclusive)).min(4096);
        let tail = self
            .airc
            .scan_agent_feed_in_channels(None, upper, limit, &self.channels)
            .await?;
        let scanned: Vec<_> = tail
            .events
            .into_iter()
            .filter(|e| key(&e.cursor()) > key(after))
            .filter(|e| !exclusive || key(&e.cursor()) < key(upper))
            .collect();
        let reached_start = scanned.len() < candidates;
        let oldest_scanned = scanned.first().map(|e| e.cursor());
        let mut found: Vec<_> = scanned
            .into_iter()
            .filter(|e| keep(e) && me.is_addressed(e))
            .collect();
        found.reverse();
        let page = window.page.max(1);
        let more = found.len().saturating_sub(page);
        found.truncate(page);
        let progress = if more > 0 {
            // Unshown requests remain in this window: resume just below the
            // oldest one emitted.
            match found.last() {
                Some(oldest_shown) => AddressedProgress::Below(oldest_shown.cursor()),
                None => AddressedProgress::Below(upper.clone()),
            }
        } else if reached_start {
            AddressedProgress::Finished
        } else {
            match oldest_scanned {
                Some(cursor) => AddressedProgress::Below(cursor),
                None => AddressedProgress::Finished,
            }
        };
        Ok(AddressedPage {
            shown: found,
            more,
            progress,
        })
    }
    /// Consumes the guard. `settle_history` records that the caller has shown what
    /// `history_summary` returned (or that it returned nothing): the count is
    /// marked shown, the addressed checkpoint moves to what was emitted, and the
    /// range settles only once no addressed request in it is left unshown. The finite commit task retains OS
    /// ownership even if its caller is cancelled while the database driver still
    /// executes the write.
    pub async fn acknowledge(self, live_end: usize, settle_history: bool) -> Result<(), AircError> {
        tokio::spawn(async move {
            let mut turn = self;
            if rooms(&turn.airc).await? != turn.state.rooms {
                return Err(fail("subscription scope changed; checkpoint retained"));
            }
            let delivered = prefix(&turn.live, live_end, &turn.state.live)?;
            if key(&delivered) < key(&turn.state.live) || key(&delivered) > key(&turn.through) {
                return Err(fail("acknowledgment outside prepared range"));
            }
            turn.state.live = delivered;
            if let (true, Some(progress)) = (settle_history, turn.progress.take()) {
                turn.state.history_counted |= progress.counted_shown;
                match progress.addressed {
                    AddressedProgress::Finished => {
                        turn.state.summarized = turn.state.boundary.clone();
                        turn.state.history_counted = false;
                        turn.state.addressed_below = None;
                    }
                    AddressedProgress::Below(cursor) => {
                        turn.state.addressed_below = Some(cursor);
                    }
                }
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
                    history_counted: false,
                    addressed_below: None,
                };
                persist(self, &consumer, &state).await?;
                state
            };
        if attaching {
            // What arrived while detached becomes this attachment's history: the
            // new boundary is the present. `live`, the delivery proof, does not
            // move here; nothing is delivered until a turn's output is
            // acknowledged. A settled consumer's next history starts where its
            // previous reading stopped (`scan_from`: what was delivered or already
            // history), so nothing is counted twice; an unsettled one keeps its
            // oldest position and its addressed checkpoint until it finishes.
            // History is never replayed as live output; `airc inbox` reads it.
            let settled = key(&state.summarized) >= key(&state.boundary);
            let read_through = state.scan_from().clone();
            if settled {
                state.summarized = read_through.clone();
                state.history_counted = false;
                state.addressed_below = None;
            }
            if key(&tip) > key(&read_through) {
                if !settled {
                    // The range grew: its count must be shown again.
                    state.history_counted = false;
                }
                state.boundary = tip.clone();
            } else {
                state.boundary = read_through;
            }
            persist(self, &consumer, &state).await?;
        }
        let scan_from = state.scan_from().clone();
        let through = if key(&scan_from) > key(&tip) {
            scan_from.clone()
        } else {
            tip
        };
        let live = self
            .scan_agent_feed_in_channels(Some(&scan_from), &through, scan_limit, &channels)
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
            progress: None,
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
    async fn summary(turn: &mut AgentDeliveryTurn, airc: &Airc) -> Option<HistorySummary> {
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
        let mut attached = airc
            .prepare_native_delivery("native", true, 64, 64)
            .await
            .unwrap();
        assert!(attached.live_events().is_empty());
        assert_eq!(
            summary(&mut attached, &airc)
                .await
                .unwrap()
                .counted
                .unwrap(),
            counted(2, 2, 3)
        );
        drop(attached); // summary not shown: it stays unsettled across reattach
        append(&airc, room, 4).await;
        let mut again = airc
            .prepare_native_delivery("native", true, 64, 64)
            .await
            .unwrap();
        assert_eq!(
            summary(&mut again, &airc)
                .await
                .unwrap()
                .counted
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
    // what this catches: card ba22c72c. A long backlog is counted by the store once,
    // with its original times (header-routed, no event read); it settles only when
    // the bounded addressed search has covered it, and never shows the count twice;
    // only events newer than attachment deliver, across a restart.
    #[tokio::test]
    async fn backlog_is_counted_once_and_settles_when_searched() {
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
        let mut initial = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(initial.live_events().is_empty());
        let found = summary(&mut initial, &airc).await.unwrap();
        assert_eq!(found.counted.unwrap(), counted(5000, 1, 5000));
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
            routed.counted.unwrap().count,
            5000 - 1,
            "header-routed out of the count"
        );
        drop(initial); // output failed: the summary stays due
        append(&airc, room, 5001).await;
        let mut first = airc
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(first.has_history());
        assert_eq!(first.live_events()[0].lamport, 5001);
        assert!(summary(&mut first, &airc).await.unwrap().counted.is_some());
        first.acknowledge(1, true).await.unwrap();
        // The interim addressed search walks one window (64) per turn, silently,
        // until it reaches the start: no turn settles past what it searched, and
        // the count is never shown again.
        let mut turns = 1;
        loop {
            let mut turn = airc
                .prepare_agent_delivery("consumer", 64, 8)
                .await
                .unwrap();
            if !turn.has_history() {
                break;
            }
            assert!(summary(&mut turn, &airc).await.is_none(), "silent walk");
            turn.acknowledge(0, true).await.unwrap();
            turns += 1;
            assert!(turns <= 5000 / 64 + 2, "the walk progresses every turn");
        }
        assert_eq!(turns, 5000 / 64 + 1);
        assert!(airc
            .load_runtime_cursor("consumer")
            .await
            .unwrap()
            .is_none()); // legacy untouched
        let reopened = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        let mut settled = reopened
            .prepare_agent_delivery("consumer", 64, 8)
            .await
            .unwrap();
        assert!(
            !settled.has_history(),
            "settled once the walk reached the start"
        );
        assert!(summary(&mut settled, &reopened).await.is_none());
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
    fn request(airc: &Airc, room: RoomId, n: u64) -> TranscriptEvent {
        TranscriptEvent {
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
        }
    }
    // what this catches (Codex on #1617): summary settlement never buries a direct
    // request. 20 addressed pre-attach requests at a page of 8 show 8, 8, then 4,
    // newest first, none lost or repeated; the range settles only after the last.
    #[tokio::test]
    async fn addressed_requests_page_across_turns_until_every_one_is_shown() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=20 {
            airc.append_event(request(&airc, room, n)).await.unwrap();
        }
        let mut pages = Vec::new();
        for turn_no in 0..3 {
            let mut turn = airc.prepare_agent_delivery("paged", 64, 8).await.unwrap();
            assert!(turn.has_history(), "turn {turn_no} still owes requests");
            let found = summary(&mut turn, &airc).await.unwrap();
            assert_eq!(found.counted.is_some(), turn_no == 0, "count shown once");
            pages.push(
                found
                    .addressed
                    .iter()
                    .map(|e| e.lamport)
                    .collect::<Vec<_>>(),
            );
            assert_eq!(found.addressed_complete, turn_no == 2);
            turn.acknowledge(0, true).await.unwrap();
        }
        assert_eq!(
            pages,
            vec![
                (13..=20).rev().collect::<Vec<_>>(),
                (5..=12).rev().collect(),
                (1..=4).rev().collect(),
            ]
        );
        let done = airc.prepare_agent_delivery("paged", 64, 8).await.unwrap();
        assert!(!done.has_history());
    }
    // what this catches (Codex on #1617): when the bounded interim search cannot see
    // the whole range, it never settles past a request it has not reached: an old
    // request under 30 newer events is shown on the turn that reaches it, and the
    // history settles only after that.
    #[tokio::test]
    async fn an_incomplete_search_never_settles_past_an_unseen_request() {
        let (_dir, airc, room) = fixture().await;
        airc.append_event(request(&airc, room, 1)).await.unwrap();
        for n in 2..=31 {
            append(&airc, room, n).await;
        }
        let narrow = AddressedWindow {
            candidates: 10,
            page: 8,
        };
        let me = airc.addressee().await.unwrap();
        let mut shown = Vec::new();
        for turn_no in 0..4 {
            let mut turn = airc.prepare_agent_delivery("narrow", 64, 8).await.unwrap();
            assert!(
                turn.has_history(),
                "not settled before the request is shown"
            );
            let found = turn
                .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, narrow)
                .await
                .unwrap();
            if let Some(found) = &found {
                assert_eq!(
                    found.addressed_complete,
                    turn_no == 3,
                    "never claims completeness early"
                );
                shown.extend(found.addressed.iter().map(|e| e.lamport));
            }
            turn.acknowledge(0, true).await.unwrap();
        }
        assert_eq!(shown, vec![1]);
        let done = airc.prepare_agent_delivery("narrow", 64, 8).await.unwrap();
        assert!(!done.has_history());
    }
    /// Walk summary turns on `consumer` until its history settles; every
    /// addressed lamport shown, in order.
    async fn walk(airc: &Airc, consumer: &str, window: AddressedWindow, attach: bool) -> Vec<u64> {
        let me = airc.addressee().await.unwrap();
        let mut shown = Vec::new();
        for turn_no in 0..64 {
            let mut turn = airc
                .prepare_native_delivery(consumer, attach && turn_no == 0, 64, 64)
                .await
                .unwrap();
            if !turn.has_history() {
                return shown;
            }
            if let Some(found) = turn
                .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, window)
                .await
                .unwrap()
            {
                shown.extend(found.addressed.iter().map(|e| e.lamport));
            }
            turn.acknowledge(0, true).await.unwrap();
        }
        panic!("history never settled");
    }
    // what this catches (Codex (a) on #1617): a request OLDER than the bounded
    // candidate tail survives a reattach that is dropped and a process restart, and
    // is still emitted.
    #[tokio::test]
    async fn an_old_request_beyond_the_tail_survives_a_dropped_reattach_and_restart() {
        let (dir, airc, room) = fixture().await;
        airc.prepare_native_delivery("old", true, 64, 64)
            .await
            .unwrap()
            .acknowledge(0, true)
            .await
            .unwrap();
        airc.append_event(request(&airc, room, 1)).await.unwrap();
        for n in 2..=81 {
            append(&airc, room, n).await; // more than the 64-event candidate tail
        }
        drop(
            airc.prepare_native_delivery("old", true, 64, 64)
                .await
                .unwrap(),
        );
        drop(airc);
        let restarted = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        assert_eq!(walk(&restarted, "old", WINDOW, true).await, vec![1]);
    }
    // what this catches (Codex (b) on #1617): the addressed checkpoint moves only
    // through the range a turn actually scanned; the unscanned rest stays unsettled.
    #[tokio::test]
    async fn the_addressed_checkpoint_moves_only_through_scanned_ranges() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=30 {
            append(&airc, room, n).await;
        }
        let narrow = AddressedWindow {
            candidates: 10,
            page: 8,
        };
        let mut turn = airc.prepare_agent_delivery("scan", 64, 8).await.unwrap();
        let start = turn.state.summarized.clone();
        let me = airc.addressee().await.unwrap();
        turn.history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, narrow)
            .await
            .unwrap();
        turn.acknowledge(0, true).await.unwrap();
        let next = airc.prepare_agent_delivery("scan", 64, 8).await.unwrap();
        assert_eq!(
            next.state.summarized, start,
            "nothing below the scan is settled"
        );
        assert_eq!(
            next.state.addressed_below.as_ref().map(|c| c.lamport),
            Some(21),
            "only 21..=30 were scanned"
        );
        assert!(next.has_history());
    }
    // what this catches (Codex (c) on #1617): showing the ambient count once does
    // not stop the addressed pages that follow it.
    #[tokio::test]
    async fn the_ambient_count_once_does_not_suppress_later_addressed_pages() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=12 {
            airc.append_event(request(&airc, room, n)).await.unwrap();
        }
        for n in 13..=20 {
            append(&airc, room, n).await;
        }
        let me = airc.addressee().await.unwrap();
        let mut seen = Vec::new();
        for turn_no in 0..2 {
            let mut turn = airc.prepare_agent_delivery("mixed", 64, 8).await.unwrap();
            let found = turn
                .history_summary(airc_core::HeaderFilter::Any, None, |_| true, &me, WINDOW)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.counted.is_some(), turn_no == 0);
            seen.push(found.addressed.len());
            turn.acknowledge(0, true).await.unwrap();
        }
        assert_eq!(seen, vec![8, 4]);
    }
    // what this catches (Codex (d) on #1617): output before acknowledgment may
    // replay after a crash and never skips: a page shown and then lost to a crash is
    // shown again, and the next page follows it.
    #[tokio::test]
    async fn a_crash_after_output_replays_the_page_and_never_skips() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=12 {
            airc.append_event(request(&airc, room, n)).await.unwrap();
        }
        let page = |found: Option<HistorySummary>| -> Vec<u64> {
            found
                .map(|f| f.addressed.iter().map(|e| e.lamport).collect())
                .unwrap_or_default()
        };
        let mut crashed = airc.prepare_agent_delivery("crash", 64, 8).await.unwrap();
        let first = page(summary(&mut crashed, &airc).await);
        drop(crashed); // output written, process gone before acknowledge
        let mut replay = airc.prepare_agent_delivery("crash", 64, 8).await.unwrap();
        assert_eq!(page(summary(&mut replay, &airc).await), first, "replayed");
        replay.acknowledge(0, true).await.unwrap();
        let mut next = airc.prepare_agent_delivery("crash", 64, 8).await.unwrap();
        assert_eq!(page(summary(&mut next, &airc).await), vec![4, 3, 2, 1]);
        assert_eq!(first, (5..=12).rev().collect::<Vec<_>>());
    }
    // what this catches (Codex on #1617): `live` is the delivery proof. A reattach
    // after an outage that is dropped before any output leaves it where it was; the
    // outage events are history, never marked delivered by the attach.
    #[tokio::test]
    async fn a_dropped_reattach_does_not_move_the_delivered_position() {
        let (_dir, airc, room) = fixture().await;
        airc.prepare_native_delivery("outage", true, 64, 64)
            .await
            .unwrap()
            .acknowledge(0, true)
            .await
            .unwrap();
        append(&airc, room, 1).await;
        let live = airc
            .prepare_native_delivery("outage", false, 64, 64)
            .await
            .unwrap();
        assert_eq!(live.live_events()[0].lamport, 1);
        live.acknowledge(1, true).await.unwrap(); // delivered through 1
        append(&airc, room, 2).await; // the outage
        append(&airc, room, 3).await;
        let reattach = airc
            .prepare_native_delivery("outage", true, 64, 64)
            .await
            .unwrap();
        assert!(
            reattach.live_events().is_empty(),
            "2..3 are history, not live"
        );
        drop(reattach); // no output, no acknowledgment
        let after = airc
            .prepare_native_delivery("outage", false, 64, 64)
            .await
            .unwrap();
        assert_eq!(after.state.live.lamport, 1, "delivered is still 1");
        assert!(after.has_history(), "2..3 remain unsettled history");
        assert!(after.live_events().is_empty());
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
        let mut turn = airc
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
        assert_eq!(
            found.counted.unwrap().count,
            2,
            "the two other-runtime sends"
        );
        let lamports: Vec<_> = found.addressed.iter().map(|e| e.lamport).collect();
        assert_eq!(
            lamports,
            [4],
            "the own request is hidden, the other runtime's shown"
        );
        let everything = summary(&mut turn, &airc).await.unwrap();
        assert_eq!(everything.counted.unwrap().count, 4);
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
        let mut settle = airc.prepare_agent_delivery("marks", 64, 8).await.unwrap();
        assert_eq!(settle.live_events()[0].lamport, 2);
        assert!(summary(&mut settle, &airc).await.is_some());
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
