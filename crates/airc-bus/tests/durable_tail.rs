//! Card 8428ae8c — `EventRouter::durable_tail`, the bounded
//! "most recent N" read behind `Inbox { since: None, limit: N }`.
//!
//! The perf claim is tested STRUCTURALLY, not by timing: the sink is
//! wrapped in a counting decorator, and a most-recent-N read on a room
//! thousands of events deep must complete with **zero `page` calls**
//! (the O(room) forward-scan primitive) and at most one `page_tail`
//! call whose requested limit is bounded by N. The previous shape
//! (`resume_from_cursor(channel, None)` + truncate) materialized the
//! WHOLE room — exactly one `page(…, None, usize::MAX)` — to answer N.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind};
use airc_bus::{
    BusError, DurableSink, EventRouter, InMemoryDurableSink, InMemoryEpochStore, ManualClock,
    RouterConfig, SeqSource,
};
use airc_core::{ClientId, EventId, PeerId, RoomId};

mod common;
use common::GatedSink;

/// A [`DurableSink`] decorator that counts how the router reads from
/// the durable tier: `page` is the O(room) forward-scan primitive,
/// `page_tail` the bounded reverse page. The tail tests assert on these
/// counters — a most-recent-N read that forward-pages even once is a
/// failed bounded-work claim.
struct CountingSink {
    inner: Arc<InMemoryDurableSink>,
    page_calls: AtomicU64,
    page_tail_calls: AtomicU64,
    max_page_tail_limit: AtomicUsize,
    max_page_limit: AtomicUsize,
}

impl CountingSink {
    fn new(inner: Arc<InMemoryDurableSink>) -> Self {
        Self {
            inner,
            page_calls: AtomicU64::new(0),
            page_tail_calls: AtomicU64::new(0),
            max_page_tail_limit: AtomicUsize::new(0),
            max_page_limit: AtomicUsize::new(0),
        }
    }

    fn max_page_limit(&self) -> usize {
        self.max_page_limit.load(Ordering::SeqCst)
    }

    fn page_calls(&self) -> u64 {
        self.page_calls.load(Ordering::SeqCst)
    }

    fn page_tail_calls(&self) -> u64 {
        self.page_tail_calls.load(Ordering::SeqCst)
    }

    fn max_page_tail_limit(&self) -> usize {
        self.max_page_tail_limit.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.page_calls.store(0, Ordering::SeqCst);
        self.page_tail_calls.store(0, Ordering::SeqCst);
        self.max_page_tail_limit.store(0, Ordering::SeqCst);
        self.max_page_limit.store(0, Ordering::SeqCst);
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
        self.max_page_limit.fetch_max(limit, Ordering::SeqCst);
        self.inner.page(channel, from_cursor, limit).await
    }

    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>, BusError> {
        self.inner.head_cursor(channel).await
    }

    async fn page_tail(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.page_tail_calls.fetch_add(1, Ordering::SeqCst);
        self.max_page_tail_limit.fetch_max(limit, Ordering::SeqCst);
        self.inner.page_tail(channel, before, limit).await
    }

    async fn page_tail_of_kinds(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        // Counted into the same reverse-page ledger: the zero-scan
        // structural proof covers the kinds-filtered leg too.
        self.page_tail_calls.fetch_add(1, Ordering::SeqCst);
        self.max_page_tail_limit.fetch_max(limit, Ordering::SeqCst);
        self.inner
            .page_tail_of_kinds(channel, before, kinds, limit)
            .await
    }

    async fn contains(&self, event_id: airc_core::EventId) -> Result<bool, BusError> {
        self.inner.contains(event_id).await
    }
}

/// A sink whose `page_tail` fails — proves the most-recent-N read
/// surfaces a store error loudly instead of falling back to the
/// forward scan (whose `page` panics to enforce exactly that).
struct FailingTailSink;

#[async_trait]
impl DurableSink for FailingTailSink {
    async fn append(&self, _e: &Envelope) -> Result<(), BusError> {
        Ok(())
    }

    async fn page(
        &self,
        _channel: RoomId,
        _from_cursor: Option<Cursor>,
        _limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        panic!("durable_tail must NEVER forward-page the room — that is the scan it replaces");
    }

    async fn head_cursor(&self, _channel: RoomId) -> Result<Option<Cursor>, BusError> {
        Ok(None)
    }

    async fn page_tail(
        &self,
        _channel: RoomId,
        _before: Option<Cursor>,
        _limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        Err(BusError::Sink("reverse index unavailable".to_string()))
    }

    async fn page_tail_of_kinds(
        &self,
        _channel: RoomId,
        _before: Option<Cursor>,
        _kinds: &[Kind],
        _limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        Err(BusError::Sink("reverse index unavailable".to_string()))
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
    kinded_event(channel, marker, Kind::Message, delivery)
}

fn kinded_event(channel: RoomId, marker: u128, kind: Kind, delivery: DeliveryClass) -> Envelope {
    Envelope::new(
        channel,
        (PeerId::from_u128(1), ClientId::from_u128(1)),
        kind,
        delivery,
        Bytes::from_static(b"tail-page"),
    )
    .with_event_id(EventId::from_u128(marker + 1))
}

fn markers(events: &[Arc<Envelope>]) -> Vec<u128> {
    events.iter().map(|e| e.event_id.0.as_u128() - 1).collect()
}

/// The bounded-work proof: most-recent-N on a room 5,000 events deep
/// (ring capacity 64, so the depth genuinely lives in the sink) must
/// do ZERO forward-scan (`page`) calls and at most ONE reverse page
/// whose requested limit never exceeds N — while returning exactly the
/// newest N in ascending order across the sink→ring seam.
#[tokio::test]
async fn most_recent_n_on_deep_room_does_bounded_store_work() {
    const DEEP: u128 = 5_000;
    const N: usize = 200;

    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0xdeeb);

    for n in 0..DEEP {
        router
            .publish(event(channel, n, DeliveryClass::Durable))
            .await
            .expect("publish");
        if n % 256 == 0 {
            // Let the write-behind drain so the bounded queue never sheds.
            tokio::task::yield_now().await;
        }
    }

    sink.reset();
    let tail = router.durable_tail(channel, N).await.expect("tail");

    let expected: Vec<u128> = (DEEP - N as u128..DEEP).collect();
    assert_eq!(
        markers(&tail),
        expected,
        "exactly the newest N, ascending, no gap and no dup at the sink→ring seam"
    );
    assert_eq!(
        sink.page_calls(),
        0,
        "5000-deep room: most-recent-N must not forward-scan the room"
    );
    assert!(
        sink.page_tail_calls() <= 1,
        "at most one reverse page, got {}",
        sink.page_tail_calls()
    );
    assert!(
        sink.max_page_tail_limit() <= N,
        "the reverse page is bounded by N, asked for {}",
        sink.max_page_tail_limit()
    );
}

/// When the hot ring alone covers N, the store is not touched at all.
#[tokio::test]
async fn ring_covered_n_does_zero_store_reads() {
    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0x717);

    for n in 0..100u128 {
        router
            .publish(event(channel, n, DeliveryClass::Durable))
            .await
            .expect("publish");
    }

    sink.reset();
    let tail = router.durable_tail(channel, 10).await.expect("tail");

    let expected: Vec<u128> = (90..100).collect();
    assert_eq!(markers(&tail), expected, "newest 10, ascending");
    assert_eq!(sink.page_calls(), 0);
    assert_eq!(
        sink.page_tail_calls(),
        0,
        "ring covers N — zero store reads"
    );
}

/// N larger than the room returns the whole room; the empty room and
/// a zero limit return nothing.
#[tokio::test]
async fn n_beyond_room_size_and_degenerate_shapes() {
    let (router, _sink) = counted_router(64);
    let channel = RoomId::from_u128(0xb16);

    // Empty room.
    assert!(
        router
            .durable_tail(channel, 50)
            .await
            .expect("tail")
            .is_empty(),
        "empty room has no tail"
    );

    for n in 0..10u128 {
        router
            .publish(event(channel, n, DeliveryClass::Durable))
            .await
            .expect("publish");
    }

    let all = router.durable_tail(channel, 100).await.expect("tail");
    let expected: Vec<u128> = (0..10).collect();
    assert_eq!(markers(&all), expected, "N > room size returns the room");

    assert!(
        router
            .durable_tail(channel, 0)
            .await
            .expect("tail")
            .is_empty(),
        "limit 0 reads nothing"
    );
}

/// `durable_tail` is the DURABLE transcript tail: newer non-durable
/// traffic (stream chunks riding the same ring) never appears in it.
#[tokio::test]
async fn non_durable_traffic_is_excluded_from_the_tail() {
    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0x5c);

    for n in 0..5u128 {
        router
            .publish(event(channel, n, DeliveryClass::Durable))
            .await
            .expect("publish durable");
    }
    for n in 5..15u128 {
        router
            .publish(event(channel, n, DeliveryClass::StreamChunk))
            .await
            .expect("publish chunk");
    }

    sink.reset();
    let tail = router.durable_tail(channel, 3).await.expect("tail");

    let expected: Vec<u128> = (2..5).collect();
    assert_eq!(
        markers(&tail),
        expected,
        "stream chunks do not ride the durable tail"
    );
    assert_eq!(sink.page_calls(), 0);
}

/// Continuum #297 — the flood case across the sink→ring seam: three
/// durable `Message`s buried under 200 newer durable `StreamChunk`s
/// (ring capacity 64, so the messages genuinely live only in the sink).
/// `durable_tail_of_kinds` must return exactly the messages — the kind
/// predicate runs BEFORE the limit on both legs — with the same bounded
/// store work as the unfiltered tail (zero forward scans, at most one
/// reverse page, requested limit ≤ N).
// what this catches: the kinds-filtered tail regressing to
// newest-N-raw-then-filter (which returns crumbs: the raw newest-50
// here is all chunks, so a persona paging Messages goes deaf to
// direction) — or the filtered leg silently forward-scanning the room.
#[tokio::test]
async fn kinds_filtered_tail_pages_newest_n_of_kind_with_bounded_work() {
    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0x297);

    for n in 0..3u128 {
        router
            .publish(event(channel, n, DeliveryClass::Durable))
            .await
            .expect("publish message");
    }
    for n in 3..203u128 {
        router
            .publish(kinded_event(
                channel,
                n,
                Kind::StreamChunk,
                DeliveryClass::Durable,
            ))
            .await
            .expect("publish durable chunk");
        if n % 64 == 0 {
            // Let the write-behind drain so the bounded queue never sheds.
            tokio::task::yield_now().await;
        }
    }

    // The flood shape this fixes: the raw newest-50 is all chunks.
    let raw = router.durable_tail(channel, 50).await.expect("raw tail");
    assert!(
        raw.iter().all(|e| e.kind == Kind::StreamChunk),
        "precondition: the raw newest-50 page is chunk-flooded"
    );

    sink.reset();
    let tail = router
        .durable_tail_of_kinds(channel, &[Kind::Message], 50)
        .await
        .expect("kinds tail");
    let expected: Vec<u128> = (0..3).collect();
    assert_eq!(
        markers(&tail),
        expected,
        "the newest 50 OF KIND Message are exactly the 3 buried messages, ascending"
    );
    assert_eq!(
        sink.page_calls(),
        0,
        "the filtered tail must not forward-scan the room"
    );
    assert!(
        sink.page_tail_calls() <= 1,
        "at most one reverse page, got {}",
        sink.page_tail_calls()
    );
    assert!(
        sink.max_page_tail_limit() <= 50,
        "the reverse page is bounded by N, asked for {}",
        sink.max_page_tail_limit()
    );

    // A limit below the match count still caps: newest 2 messages.
    let capped = router
        .durable_tail_of_kinds(channel, &[Kind::Message], 2)
        .await
        .expect("capped kinds tail");
    assert_eq!(markers(&capped), vec![1, 2], "limit caps the matches");

    // Multi-kind filter admitting every published kind == the raw tail.
    let both = router
        .durable_tail_of_kinds(channel, &[Kind::Message, Kind::StreamChunk], 50)
        .await
        .expect("multi-kind tail");
    assert_eq!(
        markers(&both),
        markers(&raw),
        "a filter admitting all kinds behaves exactly like the raw tail"
    );
}

/// §3.8 write-behind lag: a durable still pending persistence (the
/// sink hasn't seen it yet) must appear in the tail — the ring is
/// authoritative for the recent edge, the sink only lags it.
#[tokio::test]
async fn pending_unpersisted_durable_is_served_from_the_ring() {
    let inner = Arc::new(InMemoryDurableSink::new());
    let gated = Arc::new(GatedSink::new(inner));
    let epoch_store = InMemoryEpochStore::new();
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let router = EventRouter::new(
        RouterConfig {
            ring_capacity: 64,
            ..RouterConfig::default()
        },
        Arc::new(ManualClock::new(1_700_000_000_000)),
        seq,
        gated.clone(),
    );
    let channel = RoomId::from_u128(0x9e4d);

    // Appends are gated CLOSED: everything published lives only in the
    // ring (pinned un-persisted, §3.8).
    for n in 0..5u128 {
        router
            .publish(event(channel, n, DeliveryClass::Durable))
            .await
            .expect("publish");
    }

    let tail = router.durable_tail(channel, 10).await.expect("tail");
    let expected: Vec<u128> = (0..5).collect();
    assert_eq!(
        markers(&tail),
        expected,
        "un-persisted durables are in the tail — the ring leads the sink"
    );

    gated.open(); // release the write-behind so the router task drains
}

/// No-fallback contract: when the store cannot reverse-page, the read
/// fails loudly. It must never quietly degrade to materializing the
/// room (the failing sink's `page` panics to enforce that).
#[tokio::test]
async fn tail_surfaces_sink_error_instead_of_scanning() {
    let epoch_store = InMemoryEpochStore::new();
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let router = EventRouter::new(
        RouterConfig::default(),
        Arc::new(ManualClock::new(1_700_000_000_000)),
        seq,
        Arc::new(FailingTailSink),
    );

    let result = router.durable_tail(RoomId::from_u128(0xbad), 25).await;

    assert!(
        result.is_err(),
        "reverse-page failure is loud, not a scan fallback"
    );
}

/// Regression (continuum 2026-09-06, the wall projection that never
/// returned): a cursor resume must do work bounded by its page, never by
/// the channel's remaining depth. Before, `resume_from_cursor` asked the
/// sink for `usize::MAX` rows on every page and the daemon truncated in
/// memory, so a from-zero walk of a 244k-event room materialized O(n²/page)
/// rows and outlived every caller's deadline. Now: each page is ONE sink
/// read of at most `limit` rows, pages chain by the last returned cursor
/// with no gap and no dup, and a short page means the channel is exhausted.
#[tokio::test]
async fn cursor_resume_pages_are_bounded_by_limit_not_by_room_depth() {
    const DEEP: u128 = 1_200;
    const PAGE: usize = 500;

    let (router, sink) = counted_router(64);
    let channel = RoomId::from_u128(0xb0b0);
    for n in 0..DEEP {
        router
            .publish(event(channel, n, DeliveryClass::Durable))
            .await
            .expect("publish");
        if n % 256 == 0 {
            tokio::task::yield_now().await;
        }
    }

    let mut from = None;
    let mut seen: Vec<u128> = Vec::new();
    let mut pages = 0;
    loop {
        sink.reset();
        let page = router
            .resume_from_cursor(channel, from, PAGE)
            .await
            .expect("resume");
        pages += 1;
        assert!(page.len() <= PAGE, "a page never exceeds its limit");
        assert!(
            sink.page_calls() <= 1,
            "one sink read per page, got {}",
            sink.page_calls()
        );
        assert!(
            sink.max_page_limit() <= PAGE,
            "the sink read is bounded by the page, asked for {}",
            sink.max_page_limit()
        );
        let short = page.len() < PAGE;
        from = page.last().map(|e| e.cursor());
        seen.extend(markers(&page));
        if short || from.is_none() {
            break;
        }
    }
    assert_eq!(pages, 3, "1200 events at 500/page = 500 + 500 + 200");
    assert_eq!(
        seen,
        (0..DEEP).collect::<Vec<_>>(),
        "the chained pages are the whole channel in order, no gap, no dup"
    );
}
