//! Shared acceptance-test harness (§11): isolated owner instance, injectable
//! clock + seq, in-memory durable sink. The test model IS the production model
//! (§9): no global state, deterministic, explicit lifecycle.
//!
//! Each integration-test file is its own crate and pulls this module in, so a
//! given helper is "unused" from the perspective of any single binary. The
//! crate-level allow keeps the shared harness warning-free under `-D warnings`.
#![allow(dead_code)]

use std::sync::Arc;

use bytes::Bytes;

use airc_bus::{
    Clock, DeliveryClass, Envelope, EventRouter, InMemoryDurableSink, InMemoryEpochStore, Kind,
    ManualClock, RouterConfig, SeqSource,
};
use airc_core::{ClientId, EventId, PeerId, RoomId};

/// A single isolated owner-core instance plus the knobs a test drives.
pub struct Owner {
    pub router: EventRouter,
    pub clock: ManualClock,
    pub sink: Arc<InMemoryDurableSink>,
    pub epoch_store: InMemoryEpochStore,
}

impl Owner {
    /// Build an owner with the given config and a fresh epoch store / sink.
    pub fn new(config: RouterConfig) -> Self {
        let epoch_store = InMemoryEpochStore::new();
        let sink = Arc::new(InMemoryDurableSink::new());
        Self::with_parts(config, epoch_store, sink, 0)
    }

    /// Build an owner reusing a persisted epoch store + sink — models a
    /// **restart** (the new `SeqSource` bumps the epoch). `start_counter`
    /// seeds the in-memory counter (e.g. from the sink's durable max).
    pub fn with_parts(
        config: RouterConfig,
        epoch_store: InMemoryEpochStore,
        sink: Arc<InMemoryDurableSink>,
        start_counter: u64,
    ) -> Self {
        let clock = ManualClock::new(1_700_000_000_000);
        let seq = Arc::new(SeqSource::start_at_counter(&epoch_store, start_counter));
        let router = EventRouter::new(
            config,
            Arc::new(clock.clone()) as Arc<dyn Clock>,
            seq,
            sink.clone(),
        );
        Self {
            router,
            clock,
            sink,
            epoch_store,
        }
    }
}

/// Deterministic durable message envelope with a stable event_id derived from
/// `marker` so replayed copies compare equal.
pub fn durable(channel: RoomId, marker: u128, text: &str) -> Envelope {
    Envelope::new(
        channel,
        (PeerId::from_u128(1), ClientId::from_u128(1)),
        Kind::Message,
        DeliveryClass::Durable,
        Bytes::copy_from_slice(text.as_bytes()),
    )
    .with_event_id(EventId::from_u128(marker))
}

/// Deterministic ephemeral-latest envelope coalescing on `key`.
pub fn ephemeral(channel: RoomId, marker: u128, key: &str, payload: &[u8]) -> Envelope {
    Envelope::new(
        channel,
        (PeerId::from_u128(1), ClientId::from_u128(1)),
        Kind::Signal,
        DeliveryClass::EphemeralLatest,
        Bytes::copy_from_slice(payload),
    )
    .with_event_id(EventId::from_u128(marker))
    .with_coalesce_key(key)
}

// --- gated durable sink, for the no-gap eviction test ----------------------

use airc_bus::{BusError, Cursor, DurableSink};
use async_trait::async_trait;
use tokio::sync::Notify;

/// A [`DurableSink`] decorator that delays `append` until a gate is opened.
/// Lets a test hold `Durable` events "evicted-pending" (pinned in the ring,
/// not yet in the store) and then release them so the write-behind path
/// persists + unpins, after which capacity pressure evicts them — forcing a
/// later subscriber's deep-replay to fetch them from the sink (§3.8 no-gap).
pub struct GatedSink {
    inner: Arc<InMemoryDurableSink>,
    open: Arc<Notify>,
    is_open: std::sync::atomic::AtomicBool,
}

impl GatedSink {
    pub fn new(inner: Arc<InMemoryDurableSink>) -> Self {
        Self {
            inner,
            open: Arc::new(Notify::new()),
            is_open: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Open the gate — all pending and future appends proceed.
    pub fn open(&self) {
        self.is_open
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.open.notify_waiters();
    }
}

#[async_trait]
impl DurableSink for GatedSink {
    async fn append(&self, e: &Envelope) -> Result<(), BusError> {
        while !self.is_open.load(std::sync::atomic::Ordering::SeqCst) {
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
}
