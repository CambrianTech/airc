//! Live delivery with output-owned acknowledgment, and pre-attachment history
//! summarized instead of replayed.
//!
//! A consumer's first turn (and a native feed's every attachment) finds history it
//! was never attached for: everything between its last settled position and the
//! present. That history used to be paged out as "retained catch-up" a few items per
//! turn, oldest first, so a scope with months of backlog emitted months-old chat on
//! every hook call and never drained (card ba22c72c, the M5 on 2026-10-26). Now a
//! turn opens it the way a chat client opens a channel: the store's count of the
//! range with its original times, and the newest page of room events whole, once
//! ([`AgentDeliveryTurn::history_summary`]). Requests addressed to this identity
//! page on later turns until every one is shown, and only then does the range
//! settle. The history itself stays durable for `airc inbox`.
//!
//! Live delivery is oldest first, but only within what one turn can read. A
//! consumer further behind than one turn re-attaches at the present
//! (`prepare_delivery_owned`, the one owner of that decision): what it missed is
//! deliberately presented as bounded history (the count with its times, the
//! newest page whole, and every addressed request until emitted) instead of
//! being replayed in ambient live order. The re-attach never moves `live`, the
//! delivery proof: nothing it reclassified was shown.
use crate::{Airc, AircError, EventFilter, EventScan};
use airc_core::{EventId, RoomId, TranscriptCursor, TranscriptEvent};
use airc_store::StoredScopedState;
use std::{
    fs::{File, OpenOptions},
    path::Path,
};
mod addressed;
mod history;
mod state;
#[cfg(test)]
mod tests;

pub use addressed::AddressedWindow;
use history::HistoryProgress;
pub use history::HistorySummary;
use state::State;

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
                if progress.unsearched.is_empty() {
                    turn.state.summarized = turn.state.boundary.clone();
                    turn.state.history_counted = false;
                }
                turn.state.unsearched = progress.unsearched;
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
                let mut state = state;
                state.ensure_unsearched();
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
                let mut state = State {
                    version: 1,
                    live: boundary.clone(),
                    boundary,
                    summarized,
                    rooms: channels.clone(),
                    source,
                    history_counted: false,
                    unsearched: Vec::new(),
                };
                state.ensure_unsearched();
                persist(self, &consumer, &state).await?;
                state
            };
        // A consumer further behind than one turn can read re-attaches: what it missed becomes
        // history (counted, with what was addressed to it emitted oldest range first), and live
        // delivery restarts at the present; ambient live order is deliberately given up for a
        // bounded history presentation, and `live` (the delivery proof) does not move. Paged oldest first, a hook whose peers post faster
        // than its page never caught up: a Claude Code session read its 10-26 backlog on 10-29,
        // nine events a call, and every message addressed to it that day stayed out of reach.
        let attaching = attaching || {
            let probe = self
                .scan_agent_feed_in_channels(
                    Some(state.scan_from()),
                    &tip,
                    scan_limit,
                    &channels,
                    None,
                )
                .await?;
            probe
                .scanned_through
                .as_ref()
                .is_some_and(|through| key(through) < key(&tip))
        };
        if attaching {
            // What arrived while detached becomes this attachment's history: the
            // new boundary is the present. `live`, the delivery proof, does not
            // move here; nothing is delivered until a turn's output is
            // acknowledged. A settled consumer's next history starts where its
            // previous reading stopped (`scan_from`: what was delivered or already
            // history), so nothing is counted twice; an unsettled one keeps its
            // oldest position and every unsearched range it still owes. Either way
            // the new segment `(read_through, tip]` is appended as unsearched, so a
            // request that arrived during this outage is searched too.
            // History is never replayed as live output; `airc inbox` reads it.
            let settled = key(&state.summarized) >= key(&state.boundary);
            let read_through = state.scan_from().clone();
            if settled {
                state.summarized = read_through.clone();
                state.history_counted = false;
                state.unsearched.clear();
            }
            if key(&tip) > key(&read_through) {
                if !settled {
                    // The range grew: its count must be shown again.
                    state.history_counted = false;
                }
                state.unsearched.push(state::Unsearched {
                    after: read_through,
                    top: tip.clone(),
                    top_exclusive: false,
                });
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
            .scan_agent_feed_in_channels(Some(&scan_from), &through, scan_limit, &channels, None)
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
