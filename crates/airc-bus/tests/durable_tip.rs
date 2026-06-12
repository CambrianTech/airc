//! Card a1562dbc — `EventRouter::durable_tip`, the O(1) room-tip probe.
//!
//! The perf claim is tested STRUCTURALLY, not by timing: the sink is
//! wrapped in a counting decorator, and every probe must complete with
//! **zero `page` calls** (the O(n) scan primitive) regardless of how
//! deep the room's history is. The sink leg, when taken, is exactly one
//! `head_cursor` call (one indexed row on the real SQLite sink).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind};
use airc_bus::{
    BusError, DurableSink, EventRouter, InMemoryDurableSink, InMemoryEpochStore, ManualClock,
    RouterConfig, SeqSource,
};
use airc_core::{ClientId, EventId, PeerId, RoomId};

/// A [`DurableSink`] decorator that counts how the router reads from
/// the durable tier: `page` is the O(n) scan primitive, `head_cursor`
/// the O(1) index probe. The tip tests assert on these counters — a
/// probe that pages even once is a failed O(1) claim.
struct CountingSink {
    inner: Arc<InMemoryDurableSink>,
    page_calls: AtomicU64,
    head_cursor_calls: AtomicU64,
}

impl CountingSink {
    fn new(inner: Arc<InMemoryDurableSink>) -> Self {
        Self {
            inner,
            page_calls: AtomicU64::new(0),
            head_cursor_calls: AtomicU64::new(0),
        }
    }

    fn page_calls(&self) -> u64 {
        self.page_calls.load(Ordering::SeqCst)
    }

    fn head_cursor_calls(&self) -> u64 {
        self.head_cursor_calls.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.page_calls.store(0, Ordering::SeqCst);
        self.head_cursor_calls.store(0, Ordering::SeqCst);
    }
}

#[async_trait]
impl DurableSink for CountingSink {
    async fn append(&self, e: &Envelope) -> Result<(), BusError> {
        self.inner.append(e).await
    }

    async fn page(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.page_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.page(channel, from_cursor, limit).await
    }

    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>, BusError> {
        self.head_cursor_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.head_cursor(channel).await
    }

    async fn contains(&self, event_id: airc_core::EventId) -> Result<bool, BusError> {
        self.inner.contains(event_id).await
    }
}

/// A sink whose `head_cursor` fails — proves the tip probe surfaces a
/// store error loudly instead of falling back to a scan.
struct FailingHeadSink;

#[async_trait]
impl DurableSink for FailingHeadSink {
    async fn append(&self, _e: &Envelope) -> Result<(), BusError> {
        Ok(())
    }

    async fn page(
        &self,
        _channel: RoomId,
        _from_cursor: Option<Cursor>,
        _limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        panic!("durable_tip must NEVER page the room — that is the scan it replaces");
    }

    async fn head_cursor(&self, _channel: RoomId) -> Result<Option<Cursor>, BusError> {
        Err(BusError::Sink("index unavailable".to_string()))
    }

    async fn contains(&self, _event_id: airc_core::EventId) -> Result<bool, BusError> {
        Ok(false)
    }
}

/// Router over a counting sink. Small ring so deep history genuinely
/// lives only in the sink; large write-behind so a publish burst never
/// sheds.
fn counted_router(ring_capacity: usize) -> (EventRouter, Arc<CountingSink>) {
    let sink = Arc::new(CountingSink::new(Arc::new(InMemoryDurableSink::new())));
    let epoch_store = InMemoryEpochStore::new();
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let router = EventRouter::new(
        RouterConfig {
            ring_capacity,
            write_behind_buffer: 16_384,
            ..RouterConfig::default()
        },
        Arc::new(ManualClock::new(1_700_000_000_000)),
        seq,
        sink.clone(),
    );
    (router, sink)
}

fn event(channel: RoomId, marker: u128, delivery: DeliveryClass) -> Envelope {
    Envelope::new(
        channel,
        (PeerId::from_u128(1), ClientId::from_u128(1)),
        Kind::Message,
        delivery,
        Bytes::from_static(b"tip-probe"),
    )
    .with_event_id(EventId::from_u128(marker + 1))
}

#[tokio::test]
async fn empty_room_tip_is_none_via_one_index_probe() {
    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0xe);

    let tip = router.durable_tip(channel).await.expect("tip probe");

    assert_eq!(tip, None, "no history ⇒ no tip");
    assert_eq!(sink.page_calls(), 0, "an empty room must not be scanned");
    assert_eq!(sink.head_cursor_calls(), 1, "exactly one index probe");
}

#[tokio::test]
async fn single_event_room_tip_is_that_event() {
    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0x1);

    let env = event(channel, 0, DeliveryClass::Durable);
    let event_id = env.event_id;
    let seq = router.publish(env).await.expect("publish");

    sink.reset();
    let tip = router
        .durable_tip(channel)
        .await
        .expect("tip probe")
        .expect("tip exists");

    assert_eq!(tip.seq, seq, "tip seq IS the publish receipt seq");
    assert_eq!(tip.event_id, event_id);
    assert_eq!(sink.page_calls(), 0);
    assert_eq!(sink.head_cursor_calls(), 0, "served from the hot ring");
}

/// The O(1) proof: a room thousands of events deep answers its tip with
/// ZERO scan (`page`) calls — identical store work to a tiny room. The
/// ring is far smaller than the history, so the depth genuinely lives
/// in the sink; the probe still never touches the scan primitive.
#[tokio::test]
async fn tip_probe_on_deep_room_does_constant_store_work() {
    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0xdeeb);

    let mut last_seq = None;
    let mut last_event_id = None;
    for n in 0..5_000u128 {
        let env = event(channel, n, DeliveryClass::Durable);
        last_event_id = Some(env.event_id);
        last_seq = Some(router.publish(env).await.expect("publish"));
        if n % 256 == 0 {
            // Let the write-behind drain so the bounded queue never sheds.
            tokio::task::yield_now().await;
        }
    }

    sink.reset();
    let tip = router
        .durable_tip(channel)
        .await
        .expect("tip probe")
        .expect("tip exists");

    assert_eq!(Some(tip.seq), last_seq, "tip is the newest durable");
    assert_eq!(Some(tip.event_id), last_event_id);
    assert_eq!(
        sink.page_calls(),
        0,
        "5000-deep room: the tip probe must not page (scan) the room"
    );
    assert!(
        sink.head_cursor_calls() <= 1,
        "at most one index probe, got {}",
        sink.head_cursor_calls()
    );
}

#[tokio::test]
async fn tip_ignores_newer_non_durable_traffic() {
    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0x5c);

    let durable_seq = router
        .publish(event(channel, 0, DeliveryClass::Durable))
        .await
        .expect("publish durable");
    // A media burst after the last chat message: newer in the ring,
    // but not transcript — must not move the tip.
    for n in 1..10u128 {
        router
            .publish(event(channel, n, DeliveryClass::StreamChunk))
            .await
            .expect("publish chunk");
    }

    sink.reset();
    let tip = router
        .durable_tip(channel)
        .await
        .expect("tip probe")
        .expect("tip exists");

    assert_eq!(tip.seq, durable_seq, "stream chunks do not move the tip");
    assert_eq!(sink.page_calls(), 0);
}

/// Fresh-start shape: the ring holds no durable (only live non-durable
/// traffic since start), the durable history lives in the sink. The tip
/// comes from the sink's index — one `head_cursor`, zero `page`.
#[tokio::test]
async fn tip_falls_through_to_sink_index_when_ring_has_no_durable() {
    let inner = Arc::new(InMemoryDurableSink::new());
    let channel = RoomId::from_u128(0xc01d);

    // Pre-seed the durable tier (models history persisted before a
    // daemon restart). Cursor seqs are owner-assigned epoch 1.
    let mut persisted_tip = None;
    for n in 0..100u128 {
        let mut env = event(channel, n, DeliveryClass::Durable);
        env.seq = airc_bus::Seq::new(1, n as u64);
        persisted_tip = Some(env.cursor());
        inner.append(&env).await.expect("seed sink");
    }

    let sink = Arc::new(CountingSink::new(inner));
    let epoch_store = InMemoryEpochStore::new();
    // First start consumed epoch 1 (the seeded history); this source
    // models the restart and stamps epoch 2.
    let _ = SeqSource::start(&epoch_store);
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let router = EventRouter::new(
        RouterConfig::default(),
        Arc::new(ManualClock::new(1_700_000_000_000)),
        seq,
        sink.clone(),
    );

    // Only non-durable traffic since "restart" — ring has no durable.
    router
        .publish(event(channel, 0xbeef, DeliveryClass::StreamChunk))
        .await
        .expect("publish chunk");

    sink.reset();
    let tip = router
        .durable_tip(channel)
        .await
        .expect("tip probe")
        .expect("tip exists");

    assert_eq!(
        Some(tip),
        persisted_tip,
        "tip is the persisted durable head"
    );
    assert_eq!(
        sink.page_calls(),
        0,
        "the sink leg is the index, not a scan"
    );
    assert_eq!(sink.head_cursor_calls(), 1);
}

/// No-fallback contract: when the store index cannot answer, the probe
/// fails loudly. It must never quietly degrade to paging the room (the
/// failing sink's `page` panics to enforce that).
#[tokio::test]
async fn tip_probe_surfaces_sink_error_instead_of_scanning() {
    let epoch_store = InMemoryEpochStore::new();
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let router = EventRouter::new(
        RouterConfig::default(),
        Arc::new(ManualClock::new(1_700_000_000_000)),
        seq,
        Arc::new(FailingHeadSink),
    );

    let result = router.durable_tip(RoomId::from_u128(0xbad)).await;

    assert!(
        result.is_err(),
        "index failure is loud, not a scan fallback"
    );
}
