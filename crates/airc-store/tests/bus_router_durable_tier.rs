//! Integration proof — the owner-core `EventRouter` against the REAL
//! SQLite durable tier (§3.5 / §3.8 of
//! `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! This is the airc-bus `no-gap cursor` acceptance scenario
//! (`evicted_pending_durable_is_served_from_sink_not_skipped`) run with
//! [`airc_store::SqliteDurableSink`] swapped in for
//! `InMemoryDurableSink`: publish durables, hold them pinned in the ring
//! while a gate keeps the sink shut, open the gate so write-behind
//! persists + unpins them, force ring eviction, then attach-from-start
//! and assert EVERY event is delivered — the evicted ones now coming
//! from **real SQLite on disk**. It proves the owner-core works against
//! the production durable tier, not just the in-memory test sink.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use tokio::sync::Notify;

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind};
use airc_bus::{
    BusError, Clock, DurableSink, EventRouter, Filter, InMemoryEpochStore, ManualClock,
    RouterConfig, SeqSource,
};
use airc_core::{ClientId, EventId, PeerId, RoomId};
use airc_store::SqliteDurableSink;

/// A [`DurableSink`] decorator that delays `append` until a gate opens,
/// forwarding everything to a real [`SqliteDurableSink`]. Lets the test
/// hold `Durable` events "evicted-pending" (pinned in the ring, not yet
/// on disk), then release them so write-behind persists + unpins — after
/// which ring-capacity pressure evicts them and a later subscriber's
/// deep-replay must fetch them from SQLite (§3.8 no-gap).
struct GatedSqliteSink {
    inner: Arc<SqliteDurableSink>,
    open: Notify,
    is_open: AtomicBool,
}

impl GatedSqliteSink {
    fn new(inner: Arc<SqliteDurableSink>) -> Self {
        Self {
            inner,
            open: Notify::new(),
            is_open: AtomicBool::new(false),
        }
    }

    fn open(&self) {
        self.is_open.store(true, Ordering::SeqCst);
        self.open.notify_waiters();
    }
}

#[async_trait]
impl DurableSink for GatedSqliteSink {
    async fn append(&self, e: &Envelope) -> Result<(), BusError> {
        while !self.is_open.load(Ordering::SeqCst) {
            self.open.notified().await;
        }
        self.inner.append(e).await
    }

    async fn page(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.inner.page(channel, from_cursor, limit).await
    }

    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>, BusError> {
        self.inner.head_cursor(channel).await
    }

    async fn contains(&self, event_id: airc_core::EventId) -> Result<bool, BusError> {
        self.inner.contains(event_id).await
    }
}

/// Deterministic durable envelope with a stable event_id so replayed
/// copies compare equal across the ring/sink/live legs.
fn durable(channel: RoomId, marker: u128, text: &str) -> Envelope {
    Envelope::new(
        channel,
        (PeerId::from_u128(1), ClientId::from_u128(1)),
        Kind::Message,
        DeliveryClass::Durable,
        Bytes::copy_from_slice(text.as_bytes()),
    )
    .with_event_id(EventId::from_u128(marker))
}

/// Drain `n` events with a per-event timeout so a missed event fails loud
/// (a hang) rather than passing trivially.
async fn take_n<S>(mut stream: S, n: usize) -> Vec<Arc<Envelope>>
where
    S: futures::Stream<Item = Arc<Envelope>> + Unpin,
{
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for an event — a gap/miss would hang here");
        out.push(next.expect("stream ended early — missing events"));
    }
    out
}

#[tokio::test]
async fn evicted_pending_durable_is_served_from_real_sqlite_not_skipped() {
    // Use a file-backed SQLite so the deep-replay genuinely comes off
    // disk (WAL), not just process memory.
    let dir = tempfile::tempdir().expect("tempdir");
    let path: &Path = &dir.path().join("bus_events.sqlite");
    let real = Arc::new(SqliteDurableSink::open_path(path).await.expect("open sink"));
    let gated = Arc::new(GatedSqliteSink::new(real.clone()));

    let ch = RoomId::from_u128(0xeee);

    // Build the owner-core router directly against the gated REAL sink,
    // with a tiny ring so eviction bites once durables are persisted.
    let epoch_store = InMemoryEpochStore::new();
    let clock = ManualClock::new(1_700_000_000_000);
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let r = EventRouter::new(
        RouterConfig {
            ring_capacity: 2,
            ..Default::default()
        },
        Arc::new(clock) as Arc<dyn Clock>,
        seq,
        gated.clone(),
    );

    // Publish 6 durables while the sink gate is SHUT: they fan out + ring
    // but write-behind blocks on append, so all 6 stay pinned in the ring
    // (the §3.8 floor — the ring grows past its nominal 2 rather than drop
    // an unpersisted durable).
    for i in 1..=6u128 {
        r.publish(durable(ch, i, &format!("m{i}")))
            .await
            .expect("publish");
    }
    assert_eq!(
        r.pinned_in_ring(ch),
        6,
        "all unpersisted durables pinned — ring exceeds capacity (§3.8 floor)"
    );

    // Open the gate: write-behind drains into real SQLite, persists + unpins
    // all 6, and the ring evicts toward capacity. Wait until the sink holds
    // all 6 (verified via page) and the ring has shrunk.
    gated.open();
    let mut waited = 0;
    loop {
        let persisted = real.page(ch, None, 100).await.expect("page").len();
        if persisted >= 6 && r.ring_len(ch) <= 2 {
            break;
        }
        assert!(
            waited < 5000,
            "timed out waiting for persistence + eviction"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
        waited += 5;
    }
    assert_eq!(
        real.page(ch, None, 100).await.expect("page").len(),
        6,
        "all durables persisted to real SQLite after gate open"
    );
    assert!(
        r.ring_len(ch) <= 2,
        "ring evicted the now-persisted durables (so 1..=4 are NOT in RAM)"
    );

    // Attach from the beginning. Events 1..=4 are gone from the ring; they
    // MUST be served from the SQLite deep-replay leg or they'd be skipped.
    let stream = r.subscribe(Filter::channel(ch), None);
    futures::pin_mut!(stream);
    let got = take_n(&mut stream, 6).await;
    let markers: Vec<u128> = got.iter().map(|e| e.event_id.0.as_u128()).collect();
    assert_eq!(
        markers,
        vec![1, 2, 3, 4, 5, 6],
        "evicted-pending durables come from REAL SQLite — none skipped (§3.8 no-gap)"
    );

    // No-dup at the seam: no further event should be immediately available.
    let extra = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(extra.is_err(), "no duplicate at the replay→live seam");
}

#[tokio::test]
async fn router_deep_replay_after_cursor_comes_from_sqlite() {
    // A leaner proof of the same seam: publish past the ring, let everything
    // persist, then attach with a mid-stream cursor and assert the tail after
    // the cursor (held only in SQLite, evicted from the tiny ring) arrives in
    // order with no gap.
    let dir = tempfile::tempdir().expect("tempdir");
    let path: &Path = &dir.path().join("bus_events.sqlite");
    let sink = Arc::new(SqliteDurableSink::open_path(path).await.expect("open sink"));

    let ch = RoomId::from_u128(0xabc);
    let epoch_store = InMemoryEpochStore::new();
    let clock = ManualClock::new(1_700_000_000_000);
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let r = EventRouter::new(
        RouterConfig {
            ring_capacity: 2,
            ..Default::default()
        },
        Arc::new(clock) as Arc<dyn Clock>,
        seq,
        sink.clone(),
    );

    // Publish 8 durables; capture the cursor after the 3rd.
    let mut cursors = Vec::new();
    for i in 1..=8u128 {
        let s = r
            .publish(durable(ch, i, &format!("m{i}")))
            .await
            .expect("publish");
        cursors.push(Cursor::new(s, EventId::from_u128(i)));
    }

    // Wait until all 8 are in SQLite (and thus mostly evicted from the ring).
    let mut waited = 0;
    while sink.page(ch, None, 100).await.expect("page").len() < 8 {
        assert!(waited < 5000, "timed out waiting for persistence");
        tokio::time::sleep(Duration::from_millis(5)).await;
        waited += 5;
    }

    // Attach strictly after the 3rd event — expect 4..=8 from SQLite deep-replay.
    let from = cursors[2];
    let stream = r.subscribe(Filter::channel(ch), Some(from));
    futures::pin_mut!(stream);
    let got = take_n(&mut stream, 5).await;
    let markers: Vec<u128> = got.iter().map(|e| e.event_id.0.as_u128()).collect();
    assert_eq!(
        markers,
        vec![4, 5, 6, 7, 8],
        "deep-replay strictly after the cursor, in order, from SQLite"
    );
}
