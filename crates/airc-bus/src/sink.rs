//! The durable tier seam (§3.3 ORM durable tier, behind a trait).
//!
//! This crate is the in-memory hot path; the durable tier (the ORM, §3.3) is
//! **behind this trait** so `airc-bus` never depends on `airc-store`. The
//! ORM-backed impl (batched, single-writer, prepared-statement cache) lands in
//! a later slice and adapts `Envelope` ↔ `TranscriptEvent` there. Tests here
//! use [`InMemoryDurableSink`].
//!
//! Only [`crate::DeliveryClass::Durable`] envelopes ever reach a sink — the
//! efficiency keystone (§3.3 / §3.4): high-frequency ephemerals never touch
//! the DB.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;

use airc_core::RoomId;

use crate::envelope::{Cursor, Envelope};
use crate::error::Result;

/// The durable event tier. `append` persists one `Durable` envelope; `page`
/// returns events strictly *after* a cursor for cold/deep replay past the ring
/// (§3.5). Events come back in total order `(seq, event_id)`.
#[async_trait]
pub trait DurableSink: Send + Sync {
    /// Persist one `Durable` envelope. On success the event is visible to
    /// every subsequent [`DurableSink::page`] call. The router's write-behind
    /// task pins the matching ring entry until this resolves (§3.8 ring
    /// entries pinned until persisted).
    async fn append(&self, e: &Envelope) -> Result<()>;

    /// Persist a BATCH of `Durable` envelopes in ONE durable commit
    /// (group-commit): a backing store that fsyncs per transaction pays a
    /// single fsync for the whole batch instead of one per event. Same
    /// at-least-once / idempotent-on-`event_id` contract as [`append`]; on
    /// success every event is visible to subsequent [`page`](DurableSink::page)
    /// calls. The default loops [`append`] (correct, no group-commit) so
    /// in-memory / non-fsync impls need not override; fsync-backed sinks SHOULD
    /// override to commit the batch in one transaction. Empty slice = no-op.
    ///
    /// Failure is all-or-nothing from the caller's view: on `Err` the caller
    /// (the write-behind task) leaves every batch entry pinned in the ring, so
    /// the no-gap precondition (§3.8) holds and nothing is lost.
    async fn append_batch(&self, events: &[&Envelope]) -> Result<()> {
        for e in events {
            self.append(e).await?;
        }
        Ok(())
    }

    /// Return up to `limit` events on `channel` strictly *after*
    /// `from_cursor`, in total order. `from_cursor == None` means "from the
    /// beginning of the channel." This is the deep-replay leg of the cursor
    /// contract (§3.5).
    async fn page(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>>;

    /// **Card 7d5b6a65.** Return the cursor of the most-recent persisted
    /// event on `channel`, or `None` if the sink has no event for it.
    ///
    /// Used by [`crate::EventRouter::head_cursor`] to compute the
    /// live edge when the in-memory ring is empty (fresh daemon
    /// start with backlog in the sink — without this, an
    /// `AttachRequest::from_now: true` would fall through to a full
    /// transcript replay because `ring.newest_cursor()` returned None
    /// and the deep-replay leg pages from `None` (= beginning).
    ///
    /// **Card a1562dbc — required, no scan default.** This is also the
    /// sink leg of [`crate::EventRouter::durable_tip`], the O(1) room
    /// tip probe. The previous default impl paged the entire channel to
    /// find the last cursor — exactly the silent O(n) fallback that the
    /// tip probe exists to kill — so every impl must now answer in
    /// constant work (one indexed `ORDER BY … DESC LIMIT 1` row on
    /// SQLite, back-of-vec on in-memory) or fail loudly.
    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>>;

    /// **Card 8428ae8c — required, no scan default.** Return the **last**
    /// `limit` events on `channel` strictly *before* `before` (or the
    /// channel's tail when `before` is `None`), in ascending total order
    /// `(epoch, counter, event_id)`. This is the reverse-paging leg of
    /// the "most recent N" inbox path: the previous shape materialized
    /// the WHOLE channel via [`DurableSink::page`] and truncated in
    /// memory — O(room) for an answer of size N. Every impl must answer
    /// in work bounded by `limit` (SQLite: `ORDER BY … DESC LIMIT N` on
    /// the composite `(room_id, epoch, counter, event_id)` index, then
    /// reverse in memory; in-memory: a back-of-vec slice) or fail
    /// loudly. There is deliberately NO default impl: a sink that cannot
    /// reverse-page is a compile error, never a silent forward scan.
    async fn page_tail(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>>;

    /// **Card 4132f48c.** Whether an event with this `event_id` is already
    /// persisted. This is the durable leg of
    /// [`crate::EventRouter::publish_if_new`]'s idempotency check: an
    /// inbound transport frame re-injected after the router's in-memory
    /// recent-ids window rolled (daemon restart, late echo) must not
    /// re-enter the hot ring / live fan-out as a second copy.
    ///
    /// Required, no scan default (same posture as `head_cursor`, card
    /// a1562dbc): every impl answers in indexed work — one primary-key
    /// probe on SQLite, a per-bucket id check on the in-memory test
    /// sinks — or fails loudly.
    async fn contains(&self, event_id: airc_core::EventId) -> Result<bool>;
}

/// In-memory durable tier for tests. Records append count so the
/// `ephemeral-off-sink` test can assert it was called **zero** times.
/// Optionally gates appends behind a [`tokio::sync::Notify`]-style barrier so
/// the `no-gap` test can hold a `Durable` event "evicted-pending" out of the
/// store while it forces ring eviction.
#[derive(Default)]
pub struct InMemoryDurableSink {
    inner: Mutex<SinkInner>,
}

#[derive(Default)]
struct SinkInner {
    /// channel -> ordered events (kept sorted by cursor on insert).
    events: BTreeMap<u128, Vec<Envelope>>,
    /// Total successful appends — the assertion lever for ephemeral-off-sink.
    append_count: u64,
}

impl InMemoryDurableSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times [`DurableSink::append`] has successfully run. The
    /// `ephemeral-off-sink` test asserts this stays `0` for an
    /// `EphemeralLatest` firehose.
    pub fn append_count(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .append_count
    }

    /// The total max cursor across all channels, for restart-counter seeding
    /// in the crash-safe-seq test (models "rebuild counter from ORM max").
    pub fn max_counter(&self) -> u64 {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .events
            .values()
            .flat_map(|v| v.iter())
            .map(|e| e.seq.counter)
            .max()
            .unwrap_or(0)
    }

    /// Snapshot count of persisted events on a channel (test helper).
    pub fn len(&self, channel: RoomId) -> usize {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .events
            .get(&channel.0.as_u128())
            .map_or(0, |v| v.len())
    }
}

#[async_trait]
impl DurableSink for InMemoryDurableSink {
    async fn append(&self, e: &Envelope) -> Result<()> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let bucket = guard.events.entry(e.channel.0.as_u128()).or_default();
        // Idempotent on event_id — a replay/re-inject must not double-store.
        if bucket.iter().any(|x| x.event_id == e.event_id) {
            return Ok(());
        }
        bucket.push(e.clone());
        // Keep the bucket in total order so `page` is a simple scan.
        bucket.sort_by(|a, b| {
            a.seq
                .cmp(&b.seq)
                .then_with(|| a.event_id.0.cmp(&b.event_id.0))
        });
        guard.append_count += 1;
        Ok(())
    }

    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Ok(guard
            .events
            .get(&channel.0.as_u128())
            .and_then(|bucket| bucket.last())
            .map(|env| env.cursor()))
    }

    async fn page(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(bucket) = guard.events.get(&channel.0.as_u128()) else {
            return Ok(Vec::new());
        };
        let out: Vec<Envelope> = bucket
            .iter()
            .filter(|e| match &from_cursor {
                None => true,
                Some(c) => e.cursor().is_after(c),
            })
            .take(limit)
            .cloned()
            .collect();
        Ok(out)
    }

    async fn page_tail(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(bucket) = guard.events.get(&channel.0.as_u128()) else {
            return Ok(Vec::new());
        };
        // The bucket is kept in ascending total order on insert, so the
        // tail strictly before `before` is a contiguous back slice.
        let end = match &before {
            None => bucket.len(),
            Some(b) => bucket.partition_point(|e| e.cursor().is_before(b)),
        };
        let start = end.saturating_sub(limit);
        Ok(bucket[start..end].to_vec())
    }

    async fn contains(&self, event_id: airc_core::EventId) -> Result<bool> {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Ok(guard
            .events
            .values()
            .any(|bucket| bucket.iter().any(|e| e.event_id == event_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{DeliveryClass, Kind};
    use crate::Seq;
    use airc_core::{ClientId, EventId, PeerId};
    use bytes::Bytes;

    fn durable_at(channel: RoomId, counter: u64) -> Envelope {
        let mut e = Envelope::new(
            channel,
            (PeerId::from_u128(1), ClientId::from_u128(1)),
            Kind::Message,
            DeliveryClass::Durable,
            Bytes::from_static(b"x"),
        )
        .with_event_id(EventId::from_u128(counter as u128 + 1));
        e.seq = Seq::new(1, counter);
        e
    }

    #[tokio::test]
    async fn append_counts_and_pages_after_cursor() {
        let sink = InMemoryDurableSink::new();
        let ch = RoomId::from_u128(7);
        for c in 0..5 {
            sink.append(&durable_at(ch, c)).await.unwrap();
        }
        assert_eq!(sink.append_count(), 5);

        // strictly after counter 1 -> counters 2,3,4
        let from = durable_at(ch, 1).cursor();
        let page = sink.page(ch, Some(from), 100).await.unwrap();
        let counters: Vec<u64> = page.iter().map(|e| e.seq.counter).collect();
        assert_eq!(counters, vec![2, 3, 4]);
    }

    #[tokio::test]
    async fn append_is_idempotent_on_event_id() {
        let sink = InMemoryDurableSink::new();
        let ch = RoomId::from_u128(1);
        let e = durable_at(ch, 0);
        sink.append(&e).await.unwrap();
        sink.append(&e).await.unwrap();
        assert_eq!(
            sink.append_count(),
            1,
            "re-append of same event_id is a no-op"
        );
        assert_eq!(sink.len(ch), 1);
    }
}
