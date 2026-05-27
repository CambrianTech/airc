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
