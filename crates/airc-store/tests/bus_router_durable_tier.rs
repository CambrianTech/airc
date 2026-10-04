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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use tokio::sync::Notify;

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind};
use airc_bus::{
    BusError, Clock, DurableSink, EventRouter, Filter, InMemoryEpochStore, ManualClock,
    PublishIfNew, RouterConfig, SeqSource,
};
use airc_core::{ClientId, EventId, PeerId, RoomId};
use airc_diagnostics::{DiagnosticCode, DiagnosticComponent, MemoryDiagnosticSink};
use airc_store::SqliteDurableSink;

/// Fail the first batch, hold its retry, then forward actual group commits to
/// SQLite. Notifications expose operation boundaries without timing guesses.
struct GatedSqliteSink {
    inner: Arc<SqliteDurableSink>,
    open: Notify,
    is_open: AtomicBool,
    attempts: AtomicUsize,
    attempted: Notify,
    batches: Mutex<Vec<Vec<EventId>>>,
    committed: AtomicUsize,
    commit_finished: Notify,
}

impl GatedSqliteSink {
    fn new(inner: Arc<SqliteDurableSink>) -> Self {
        Self {
            inner,
            open: Notify::new(),
            is_open: AtomicBool::new(false),
            attempts: AtomicUsize::new(0),
            attempted: Notify::new(),
            batches: Mutex::new(Vec::new()),
            committed: AtomicUsize::new(0),
            commit_finished: Notify::new(),
        }
    }

    fn open(&self) {
        self.is_open.store(true, Ordering::SeqCst);
        self.open.notify_waiters();
    }

    async fn wait_open(&self) {
        loop {
            let opened = self.open.notified();
            tokio::pin!(opened);
            opened.as_mut().enable();
            if self.is_open.load(Ordering::SeqCst) {
                return;
            }
            opened.await;
        }
    }

    async fn wait_for_attempts(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.attempts.load(Ordering::SeqCst) < count {
                self.attempted.notified().await;
            }
        })
        .await
        .expect("writer reaches the requested batch attempt");
    }

    async fn wait_for_commits(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.committed.load(Ordering::SeqCst) < count {
                self.commit_finished.notified().await;
            }
        })
        .await
        .expect("accepted durables reach the actual SQLite group commit");
    }
}

#[async_trait]
impl DurableSink for GatedSqliteSink {
    async fn append(&self, e: &Envelope) -> Result<(), BusError> {
        self.wait_open().await;
        self.inner.append(e).await
    }

    async fn append_batch(&self, events: &[&Envelope]) -> Result<(), BusError> {
        self.batches
            .lock()
            .expect("batch trace")
            .push(events.iter().map(|event| event.event_id).collect());
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        self.attempted.notify_one();
        if attempt == 1 {
            return Err(BusError::Sink("injected first group-commit failure".into()));
        }
        self.wait_open().await;
        self.inner.append_batch(events).await?;
        self.committed.fetch_add(events.len(), Ordering::SeqCst);
        self.commit_finished.notify_one();
        Ok(())
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

    async fn page_tail(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.inner.page_tail(channel, before, limit).await
    }

    async fn page_tail_of_kinds(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.inner
            .page_tail_of_kinds(channel, before, kinds, limit)
            .await
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
    // One file-backed owner follows failure, bounded admission, recovery and
    // replay. The current-thread runtime lets the initial synchronous publish
    // burst fill the queue before the one writer forms its first batch.
    let dir = tempfile::tempdir().expect("tempdir");
    let path: &Path = &dir.path().join("bus_events.sqlite");
    let real = Arc::new(SqliteDurableSink::open_path(path).await.expect("open sink"));
    let gated = Arc::new(GatedSqliteSink::new(real.clone()));
    let ch = RoomId::from_u128(0xeee);
    let epoch_store = InMemoryEpochStore::new();
    let clock = ManualClock::new(1_700_000_000_000);
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let diagnostics = Arc::new(MemoryDiagnosticSink::default());
    let r = EventRouter::new_with_diagnostics(
        RouterConfig {
            ring_capacity: 2,
            write_behind_buffer: 4,
            ..Default::default()
        },
        Arc::new(clock) as Arc<dyn Clock>,
        seq,
        gated.clone(),
        diagnostics.clone(),
    );
    let durable_filter = Filter::channel(ch).with_delivery(vec![DeliveryClass::Durable]);
    let (live, lag) = r.subscribe_live_with_lag(durable_filter.clone());
    futures::pin_mut!(live);

    for i in 1..=4u128 {
        r.publish(durable(ch, i, &format!("m{i}")))
            .await
            .expect("publish");
    }
    // The first actual append_batch fails. Its retry must occur with no new
    // publish to wake it, and remains held before touching SQLite.
    gated.wait_for_attempts(2).await;
    let initial: Vec<_> = (1..=4).map(EventId::from_u128).collect();
    assert_eq!(
        *gated.batches.lock().expect("batch trace"),
        vec![initial.clone(), initial],
        "retry owns the same batch, not a later queue drain"
    );
    assert!(real.page(ch, None, 100).await.expect("page").is_empty());
    let failures = diagnostics.events();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].code, DiagnosticCode::WriteBehindBatchFailed);
    assert_eq!(failures[0].component, DiagnosticComponent::Persistence);
    for (key, value) in [
        ("failed_attempts", "1"),
        ("batch_len", "4"),
        ("pinned_count", "4"),
        ("retry_ms", "100"),
    ] {
        assert_eq!(failures[0].fields.get(key).map(String::as_str), Some(value));
    }

    // Queue capacity excludes the held in-flight batch. Exactly four newer
    // events fit; both ordinary and deduplicating admission then refuse before
    // mutating the cursor, ring, subscriber or recent-ID state.
    for i in 5..=8u128 {
        r.publish(durable(ch, i, &format!("m{i}")))
            .await
            .expect("queue newer durable");
    }
    let before = r.head_cursor(ch);
    assert!(matches!(
        r.publish(durable(ch, 9, "refused ordinary publish")).await,
        Err(BusError::WriteBehindSaturated)
    ));
    assert!(matches!(
        r.publish_if_new(durable(ch, 10, "refused deduplicating publish"))
            .await,
        Err(BusError::WriteBehindSaturated)
    ));
    assert_eq!(r.head_cursor(ch), before);
    assert_eq!(r.pinned_in_ring(ch), 8);
    assert_eq!(r.ring_len(ch), 8);
    assert_eq!(r.shed_count(), 2);
    assert_eq!(gated.attempts.load(Ordering::SeqCst), 2);
    let accepted = take_n(&mut live, 8).await;
    assert_eq!(
        accepted
            .iter()
            .map(|env| env.event_id.0.as_u128())
            .collect::<Vec<_>>(),
        (1..=8).collect::<Vec<_>>()
    );

    // Lossy traffic cannot make an old failed durable pin an unbounded suffix.
    // Filter the live consumer to durable events so this tests retention, not
    // an unrelated subscriber overflow from the flood.
    for i in 0..384u128 {
        let mut event = durable(ch, 1_000 + i, "non-durable during failed commit");
        (event.kind, event.delivery) = match i % 3 {
            0 => (Kind::CommandResult, DeliveryClass::RequestResponse),
            1 => (Kind::StreamChunk, DeliveryClass::StreamChunk),
            _ => (Kind::Event, DeliveryClass::EphemeralWindow),
        };
        r.publish(event).await.expect("non-durable stays available");
        assert_eq!(r.pinned_in_ring(ch), 8);
        assert_eq!(r.ring_len(ch), 8, "only the accepted durable floor remains");
    }
    assert!(!lag.is_lagged());
    let pending_replay = r.subscribe(durable_filter.clone(), None);
    futures::pin_mut!(pending_replay);
    assert_eq!(
        take_n(&mut pending_replay, 8)
            .await
            .iter()
            .map(|env| env.event_id.0.as_u128())
            .collect::<Vec<_>>(),
        (1..=8).collect::<Vec<_>>(),
        "every accepted durable is replayable while SQLite still lacks it"
    );

    gated.open();
    gated.wait_for_commits(8).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.pinned_in_ring(ch) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successful commit releases every durable pin");
    let batches = gated.batches.lock().expect("batch trace").clone();
    assert_eq!(batches.len(), 3, "failed batch retries before newer work");
    assert_eq!(batches[0], batches[1]);
    assert_eq!(
        batches[2],
        (5..=8).map(EventId::from_u128).collect::<Vec<_>>()
    );
    assert!(batches.iter().all(|batch| batch.len() <= 64));
    let persisted = real.page(ch, None, 100).await.expect("real SQLite history");
    assert_eq!(
        persisted
            .iter()
            .map(|env| env.event_id.0.as_u128())
            .collect::<Vec<_>>(),
        (1..=8).collect::<Vec<_>>(),
        "all accepted IDs persisted once; refusals and lossy flood did not"
    );
    assert!(
        r.ring_len(ch) <= 2,
        "the ring shrinks after the writer releases the failed batch's floor"
    );
    let events = diagnostics.events();
    assert_eq!(events.len(), 2);
    let recovered = &events[1];
    assert_eq!(recovered.code, DiagnosticCode::WriteBehindBatchRecovered);
    assert_eq!(recovered.component, DiagnosticComponent::Persistence);
    for (key, value) in [
        ("failed_attempts", "1"),
        ("batch_len", "4"),
        ("pinned_count", "4"),
    ] {
        assert_eq!(recovered.fields.get(key).map(String::as_str), Some(value));
    }

    // Attach after shrink: the evicted prefix now comes from SQLite. Keep both
    // replay streams alive through subsequent events to exercise their seams.
    let stream = r.subscribe(durable_filter, None);
    futures::pin_mut!(stream);
    let got = take_n(&mut stream, 8).await;
    let markers: Vec<u128> = got.iter().map(|e| e.event_id.0.as_u128()).collect();
    assert_eq!(markers, (1..=8).collect::<Vec<_>>());

    // Retry the previously refused identities through the real deduplication
    // API. Their first visible copies must come now, never from the refusal or
    // persistence retry. A final ordered live sentinel makes duplicates fail
    // by identity instead of relying on a quiet-period timeout.
    for marker in [9, 10, 11] {
        assert!(matches!(
            r.publish_if_new(durable(ch, marker, "accepted after recovery"))
                .await
                .expect("retry after capacity returns"),
            PublishIfNew::Published(_)
        ));
        for received in [
            take_n(&mut live, 1).await,
            take_n(&mut pending_replay, 1).await,
            take_n(&mut stream, 1).await,
        ] {
            assert_eq!(received[0].event_id, EventId::from_u128(marker));
        }
    }
    gated.wait_for_commits(11).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.pinned_in_ring(ch) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("post-recovery durables also unpin");
    assert_eq!(
        real.page(ch, None, 100)
            .await
            .expect("complete SQLite history")
            .iter()
            .map(|env| env.event_id.0.as_u128())
            .collect::<Vec<_>>(),
        (1..=11).collect::<Vec<_>>()
    );
    assert!(r.ring_len(ch) <= 2);
    assert!(!lag.is_lagged());
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
