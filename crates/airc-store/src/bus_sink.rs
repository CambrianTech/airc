//! SQLite-backed [`airc_bus::DurableSink`] — the real ORM durable tier
//! (§3.3 of `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! This replaces the in-memory test sink (`airc_bus::InMemoryDurableSink`)
//! with a SeaORM-backed SQLite store so the owner-core's `Durable`
//! envelopes persist and replay from disk. The owner daemon is the
//! **single writer** of this store (§3.3) — there is no write-lock
//! contention — and the connection runs in **WAL** mode so reads
//! (deep-replay) never block the writer.
//!
//! ## The one sanctioned serialize/deserialize copy
//!
//! [`to_active_model`] / [`from_model`] are the ONLY place an
//! `Envelope` is mapped to/from its persisted row: `headers` → JSON,
//! `target` → JSON, `payload` → BLOB, `kind`/`delivery` enums → text,
//! `seq = (epoch, counter)` → two `i64` columns. No other copy of this
//! mapping exists; the hot path stays zero-copy (§3.1) and only the
//! durable boundary pays this cost.
//!
//! ## Order & paging
//!
//! [`DurableSink::page`] returns rows on a channel strictly *after* a
//! cursor, in the generational total order `(epoch, counter, event_id)`
//! (§3.5, §3.8 — NOT a single lamport), **bounded by `limit`**, served
//! by the composite index from a single B-tree range scan (no
//! full-table scan).

use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;
use sea_orm::{
    sea_query::{Expr, OnConflict},
    ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind, Target};
use airc_bus::{BusError, DurableSink, Seq};
use airc_core::{ClientId, EventId, Headers, PeerId, RoomId};

use crate::entities::bus_event;

/// SQLite-backed durable tier for the owner-core (§3.3).
///
/// Single-writer (the daemon owns it) + WAL — exactly the SQLite shape
/// that makes the durable tier fast (§3.3): no write-lock contention,
/// readers never block the writer.
pub struct SqliteDurableSink {
    db: DatabaseConnection,
}

impl SqliteDurableSink {
    /// Open (or create) a SQLite database at `db_url`, run migrations,
    /// and put the connection in WAL mode.
    ///
    /// For an in-memory store use `"sqlite::memory:"`; for a file store
    /// use `"sqlite://<path>?mode=rwc"`.
    pub async fn open(db_url: &str) -> Result<Self, BusError> {
        let mut opts = ConnectOptions::new(db_url.to_owned());
        // Single writer (the daemon). One connection avoids SQLite
        // write-lock contention entirely (§3.3).
        opts.connect_timeout(std::time::Duration::from_secs(5))
            .acquire_timeout(std::time::Duration::from_secs(5))
            .max_connections(1);
        let db = Database::connect(opts)
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?;
        set_wal(&db).await?;
        // Forward-compatible both directions: tolerate a DB migrated
        // ahead of this binary (version skew must not brick the sink).
        crate::migration::apply_migrations(&db)
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?;
        Ok(Self { db })
    }

    /// Open a file-backed durable tier from a filesystem path. Keeps
    /// platform path rules (Windows URI slashes) out of consumers.
    pub async fn open_path(path: &Path) -> Result<Self, BusError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| BusError::Sink(e.to_string()))?;
            }
        }
        Self::open(&sqlite_file_url(path)).await
    }

    /// Open an ephemeral in-memory durable tier. Convenience for tests.
    pub async fn in_memory() -> Result<Self, BusError> {
        Self::open("sqlite::memory:").await
    }

    /// Bump the persisted generational epoch on **this** ORM and return
    /// the [`SqliteEpochStore`] capturing the new value. Run once per
    /// daemon start. Sharing the sink's connection keeps the durable
    /// transcript (`bus_event`) and the epoch cell (`bus_epoch`) in one
    /// ORM — the single-writer machine store (§3.3 / §3.8).
    pub async fn bump_epoch(&self) -> Result<crate::SqliteEpochStore, BusError> {
        crate::SqliteEpochStore::bump(&self.db).await
    }
}

#[async_trait]
impl DurableSink for SqliteDurableSink {
    async fn append(&self, e: &Envelope) -> Result<(), BusError> {
        // Idempotent insert on `event_id` (§3.3): a replay / re-inject
        // must not double-store. ON CONFLICT DO NOTHING — a duplicate is
        // a silent no-op, never an error (the router treats append as
        // at-least-once).
        let active = to_active_model(e)?;
        let res = bus_event::Entity::insert(active)
            .on_conflict(
                OnConflict::column(bus_event::Column::EventId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await;
        match res {
            Ok(_) => Ok(()),
            // `RecordNotInserted` is the DO-NOTHING path: the event_id
            // already exists. Idempotent success, not an error.
            Err(sea_orm::DbErr::RecordNotInserted) => Ok(()),
            Err(err) => Err(BusError::Sink(err.to_string())),
        }
    }

    /// Group-commit: one multi-row INSERT … ON CONFLICT DO NOTHING for the whole
    /// batch = ONE transaction = ONE fsync (vs one per event in `append`). Same
    /// idempotent-on-`event_id` contract. SQLite executes a multi-VALUES insert
    /// atomically, so on error nothing is committed and the caller re-pins the
    /// whole batch (no partial-persist ambiguity).
    async fn append_batch(&self, events: &[&Envelope]) -> Result<(), BusError> {
        if events.is_empty() {
            return Ok(());
        }
        let actives = events
            .iter()
            .map(|e| to_active_model(e))
            .collect::<Result<Vec<_>, _>>()?;
        let res = bus_event::Entity::insert_many(actives)
            .on_conflict(
                OnConflict::column(bus_event::Column::EventId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await;
        match res {
            Ok(_) => Ok(()),
            // Every row in the batch already existed (all conflicted) — the
            // DO-NOTHING all-dupes path. Idempotent success, not an error.
            Err(sea_orm::DbErr::RecordNotInserted) => Ok(()),
            Err(err) => Err(BusError::Sink(err.to_string())),
        }
    }

    async fn page(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        // Bounded by `limit`. The trait caller (the router's deep-replay)
        // may pass a very large limit to mean "the whole tail"; clamp to
        // `i64::MAX` so the SQL bind never overflows and we never use
        // `usize::MAX` as a sentinel. The query stays indexed: ORDER BY
        // rides the composite index, LIMIT only caps the row count.
        let bounded = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .min(i64::MAX as u64);

        let mut query =
            bus_event::Entity::find().filter(bus_event::Column::RoomId.eq(channel.as_uuid()));

        // Strictly after the cursor in generational order:
        //   epoch > c.epoch
        //   OR (epoch == c.epoch AND counter > c.counter)
        //   OR (epoch == c.epoch AND counter == c.counter
        //       AND event_id > c.event_id).
        // event_id is the deterministic tiebreaker (§3.5); within one
        // epoch the monotonic counter forbids a tie, but the tiebreak
        // keeps the order total independent of how `seq` was produced.
        if let Some(c) = from_cursor {
            let epoch = c.seq.epoch as i64;
            let counter = c.seq.counter as i64;
            let event_id = c.event_id.as_uuid();
            let strictly_after = Expr::col(bus_event::Column::Epoch)
                .gt(epoch)
                .or(Expr::col(bus_event::Column::Epoch)
                    .eq(epoch)
                    .and(Expr::col(bus_event::Column::Counter).gt(counter)))
                .or(Expr::col(bus_event::Column::Epoch)
                    .eq(epoch)
                    .and(Expr::col(bus_event::Column::Counter).eq(counter))
                    .and(Expr::col(bus_event::Column::EventId).gt(event_id)));
            query = query.filter(strictly_after);
        }

        let rows = query
            .order_by_asc(bus_event::Column::Epoch)
            .order_by_asc(bus_event::Column::Counter)
            .order_by_asc(bus_event::Column::EventId)
            .limit(bounded)
            .all(&self.db)
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?;

        rows.into_iter().map(from_model).collect()
    }

    /// Card 8428ae8c: the reverse-paging leg of "most recent N". One
    /// indexed query — `ORDER BY (epoch, counter, event_id) DESC
    /// LIMIT n` rides the same composite
    /// `(room_id, epoch, counter, event_id)` index as [`Self::page`]
    /// (SQLite B-tree indexes serve both scan directions) — then the
    /// page is reversed in memory so callers get ascending total
    /// order. Cost is bounded by `limit`, never by channel depth.
    async fn page_tail(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        // Same bind clamp as `page`: a huge limit means "the whole
        // tail", never a `usize::MAX` sentinel reaching SQL.
        let bounded = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .min(i64::MAX as u64);

        let mut query =
            bus_event::Entity::find().filter(bus_event::Column::RoomId.eq(channel.as_uuid()));

        // Strictly before the cursor in generational order — the exact
        // mirror of `page`'s strictly-after predicate:
        //   epoch < c.epoch
        //   OR (epoch == c.epoch AND counter < c.counter)
        //   OR (epoch == c.epoch AND counter == c.counter
        //       AND event_id < c.event_id).
        if let Some(c) = before {
            let epoch = c.seq.epoch as i64;
            let counter = c.seq.counter as i64;
            let event_id = c.event_id.as_uuid();
            let strictly_before = Expr::col(bus_event::Column::Epoch)
                .lt(epoch)
                .or(Expr::col(bus_event::Column::Epoch)
                    .eq(epoch)
                    .and(Expr::col(bus_event::Column::Counter).lt(counter)))
                .or(Expr::col(bus_event::Column::Epoch)
                    .eq(epoch)
                    .and(Expr::col(bus_event::Column::Counter).eq(counter))
                    .and(Expr::col(bus_event::Column::EventId).lt(event_id)));
            query = query.filter(strictly_before);
        }

        let rows = query
            .order_by_desc(bus_event::Column::Epoch)
            .order_by_desc(bus_event::Column::Counter)
            .order_by_desc(bus_event::Column::EventId)
            .limit(bounded)
            .all(&self.db)
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?;

        // DESC off the index, ascending to the caller.
        rows.into_iter().rev().map(from_model).collect()
    }

    /// Card 7d5b6a65: efficient head-cursor query for the
    /// `AttachRequest::from_now` path. The trait's default impl pages
    /// the entire channel to find the last cursor; the SQLite override
    /// picks the single row at the top of total order via the existing
    /// composite index — bounded cost regardless of channel depth.
    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>, BusError> {
        let row = bus_event::Entity::find()
            .filter(bus_event::Column::RoomId.eq(channel.as_uuid()))
            .order_by_desc(bus_event::Column::Epoch)
            .order_by_desc(bus_event::Column::Counter)
            .order_by_desc(bus_event::Column::EventId)
            .one(&self.db)
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?;
        Ok(row.map(|r| {
            Cursor::new(
                airc_bus::Seq::new(r.epoch as u64, r.counter as u64),
                airc_core::EventId(r.event_id),
            )
        }))
    }

    /// Card 4132f48c: one indexed primary-key probe (`event_id` is the
    /// PK) — the durable leg of `EventRouter::publish_if_new`.
    async fn contains(&self, event_id: EventId) -> Result<bool, BusError> {
        let row = bus_event::Entity::find_by_id(event_id.as_uuid())
            .one(&self.db)
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?;
        Ok(row.is_some())
    }
}

/// Put the connection in WAL journal mode (§3.3). WAL lets the
/// deep-replay reads run concurrently with the single writer without
/// blocking it; `synchronous=NORMAL` is the standard WAL durability
/// trade (an fsync per checkpoint, not per commit) and is safe for the
/// deliver-first / persist-async contract (§3.3 — the ORM is the source
/// of truth; a crash loses only the un-flushed tail, replayable from
/// peers). In-memory databases ignore the pragma.
async fn set_wal(db: &DatabaseConnection) -> Result<(), BusError> {
    use sea_orm::{ConnectionTrait, Statement};
    let backend = db.get_database_backend();
    for pragma in ["PRAGMA journal_mode=WAL;", "PRAGMA synchronous=NORMAL;"] {
        db.execute(Statement::from_string(backend, pragma.to_owned()))
            .await
            .map_err(|e| BusError::Sink(e.to_string()))?;
    }
    Ok(())
}

/// The ONE sanctioned `Envelope -> row` serialize (§3.3). Lives only
/// here; no other copy of this mapping exists.
fn to_active_model(e: &Envelope) -> Result<bus_event::ActiveModel, BusError> {
    Ok(bus_event::ActiveModel {
        event_id: Set(e.event_id.as_uuid()),
        room_id: Set(e.channel.as_uuid()),
        epoch: Set(u64_to_i64("bus_events.epoch", e.seq.epoch)?),
        counter: Set(u64_to_i64("bus_events.counter", e.seq.counter)?),
        kind: Set(kind_to_text(e.kind).to_owned()),
        delivery: Set(delivery_to_text(e.delivery).to_owned()),
        target: Set(serde_json::to_value(&e.target).map_err(codec)?),
        correlation_id: Set(e.correlation_id),
        coalesce_key: Set(e.coalesce_key.clone()),
        headers: Set(serde_json::to_value(&e.headers).map_err(codec)?),
        payload: Set(e.payload.to_vec()),
        peer_id: Set(e.from.0.as_uuid()),
        client_id: Set(e.from.1.as_uuid()),
        occurred_at_ms: Set(u64_to_i64("bus_events.occurred_at_ms", e.occurred_at_ms)?),
    })
}

/// The ONE sanctioned `row -> Envelope` deserialize (§3.3). Inverse of
/// [`to_active_model`].
fn from_model(m: bus_event::Model) -> Result<Envelope, BusError> {
    let target: Target = serde_json::from_value(m.target).map_err(codec)?;
    let headers: Headers = serde_json::from_value(m.headers).map_err(codec)?;
    Ok(Envelope {
        event_id: EventId::from_uuid(m.event_id),
        channel: RoomId::from_uuid(m.room_id),
        from: (
            PeerId::from_uuid(m.peer_id),
            ClientId::from_uuid(m.client_id),
        ),
        target,
        kind: kind_from_text(&m.kind)?,
        delivery: delivery_from_text(&m.delivery)?,
        seq: Seq::new(
            i64_to_u64("bus_events.epoch", m.epoch)?,
            i64_to_u64("bus_events.counter", m.counter)?,
        ),
        occurred_at_ms: i64_to_u64("bus_events.occurred_at_ms", m.occurred_at_ms)?,
        correlation_id: m.correlation_id,
        coalesce_key: m.coalesce_key,
        headers,
        payload: Bytes::from(m.payload),
    })
}

fn kind_to_text(kind: Kind) -> &'static str {
    match kind {
        Kind::Message => "message",
        Kind::Event => "event",
        Kind::Command => "command",
        Kind::CommandResult => "command_result",
        Kind::Signal => "signal",
        Kind::StreamChunk => "stream_chunk",
        Kind::Control => "control",
    }
}

fn kind_from_text(text: &str) -> Result<Kind, BusError> {
    match text {
        "message" => Ok(Kind::Message),
        "event" => Ok(Kind::Event),
        "command" => Ok(Kind::Command),
        "command_result" => Ok(Kind::CommandResult),
        "signal" => Ok(Kind::Signal),
        "stream_chunk" => Ok(Kind::StreamChunk),
        "control" => Ok(Kind::Control),
        other => Err(BusError::Sink(format!("unknown bus_events.kind: {other}"))),
    }
}

fn delivery_to_text(delivery: DeliveryClass) -> &'static str {
    match delivery {
        DeliveryClass::Durable => "durable",
        DeliveryClass::EphemeralLatest => "ephemeral_latest",
        DeliveryClass::EphemeralWindow => "ephemeral_window",
        DeliveryClass::RequestResponse => "request_response",
        DeliveryClass::StreamChunk => "stream_chunk",
    }
}

fn delivery_from_text(text: &str) -> Result<DeliveryClass, BusError> {
    match text {
        "durable" => Ok(DeliveryClass::Durable),
        "ephemeral_latest" => Ok(DeliveryClass::EphemeralLatest),
        "ephemeral_window" => Ok(DeliveryClass::EphemeralWindow),
        "request_response" => Ok(DeliveryClass::RequestResponse),
        "stream_chunk" => Ok(DeliveryClass::StreamChunk),
        other => Err(BusError::Sink(format!(
            "unknown bus_events.delivery: {other}"
        ))),
    }
}

fn codec(e: serde_json::Error) -> BusError {
    BusError::Sink(format!("envelope codec error: {e}"))
}

fn u64_to_i64(field: &'static str, value: u64) -> Result<i64, BusError> {
    i64::try_from(value).map_err(|_| BusError::Sink(format!("{field} out of i64 range: {value}")))
}

fn i64_to_u64(field: &'static str, value: i64) -> Result<u64, BusError> {
    u64::try_from(value).map_err(|_| BusError::Sink(format!("{field} out of u64 range: {value}")))
}

fn sqlite_file_url(path: &Path) -> String {
    let raw = normalise_sqlite_path(path);
    if has_windows_drive_prefix(&raw) {
        format!("sqlite:{raw}?mode=rwc")
    } else {
        format!("sqlite://{raw}?mode=rwc")
    }
}

fn normalise_sqlite_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    raw.strip_prefix("//?/").unwrap_or(&raw).to_string()
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_bus::envelope::{DeliveryClass, Kind};
    use airc_core::{ClientId, EventId, PeerId, RoomId};
    use bytes::Bytes;
    use uuid::Uuid;

    /// Build a `Durable` envelope at a given `(epoch, counter)` with a
    /// deterministic `event_id` derived from the position so tests can
    /// assert exact identity and order. `event_id` is the PK, so (as with
    /// real random UUIDs) it must be globally unique across channels —
    /// the channel is mixed into the high half.
    fn durable_at(channel: RoomId, epoch: u64, counter: u64) -> Envelope {
        let mut e = Envelope::new(
            channel,
            (PeerId::from_u128(0xa1), ClientId::from_u128(0xc1)),
            Kind::Message,
            DeliveryClass::Durable,
            Bytes::from(format!("payload-{epoch}-{counter}")),
        )
        .with_event_id(EventId::from_u128(
            // Distinct per (channel, epoch, counter): channel + epoch in
            // the high half, counter in the low half, +1 so no id is nil.
            ((channel.as_uuid().as_u128() ^ (u128::from(epoch) << 16)) << 64)
                | u128::from(counter).wrapping_add(1),
        ))
        .with_header("h", format!("{epoch}:{counter}"));
        e.seq = Seq::new(epoch, counter);
        e.occurred_at_ms = 1_700_000_000_000 + counter;
        e
    }

    #[tokio::test]
    async fn append_then_page_round_trips_full_envelope() {
        // The minimum-viable proof: write one, read it back, every field
        // (incl. opaque payload + headers + target enum) intact.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let ch = RoomId::from_u128(0xc0ffee);
        let e = durable_at(ch, 1, 0)
            .with_target(Target::Peer(PeerId::from_u128(0x99)))
            .with_correlation_id(Uuid::from_u128(0x1234))
            .with_coalesce_key("k");
        sink.append(&e).await.unwrap();

        let page = sink.page(ch, None, 100).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0], e, "full envelope round-trips through the row");
    }

    #[tokio::test]
    async fn page_orders_by_epoch_counter_event_id() {
        // §3.8 crash-safe order: a post-crash event (higher epoch, lower
        // counter) sorts strictly AFTER a pre-crash event (lower epoch,
        // higher counter). Ordering must be (epoch, counter, event_id),
        // NOT a single lamport — append out of order, expect sorted.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let ch = RoomId::from_u128(7);
        // epoch 2 counter 0 appended FIRST but must sort LAST after
        // epoch 1's counters; epoch 1 counter 2 appended before counter 1.
        sink.append(&durable_at(ch, 2, 0)).await.unwrap();
        sink.append(&durable_at(ch, 1, 2)).await.unwrap();
        sink.append(&durable_at(ch, 1, 0)).await.unwrap();
        sink.append(&durable_at(ch, 1, 1)).await.unwrap();

        let page = sink.page(ch, None, 100).await.unwrap();
        let order: Vec<(u64, u64)> = page.iter().map(|e| (e.seq.epoch, e.seq.counter)).collect();
        assert_eq!(
            order,
            vec![(1, 0), (1, 1), (1, 2), (2, 0)],
            "epoch dominates; post-crash epoch sorts after pre-crash even with lower counter"
        );
    }

    #[tokio::test]
    async fn append_is_idempotent_on_event_id() {
        // §3.3: a replay / re-inject of the same event_id is a no-op,
        // never a duplicate row, never an error.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let ch = RoomId::from_u128(1);
        let e = durable_at(ch, 1, 0);
        sink.append(&e).await.unwrap();
        sink.append(&e).await.unwrap();
        sink.append(&e).await.unwrap();

        let page = sink.page(ch, None, 100).await.unwrap();
        assert_eq!(page.len(), 1, "same event_id => exactly one row");
    }

    #[tokio::test]
    async fn page_returns_strictly_after_cursor_and_respects_limit() {
        // Cursor pagination (§3.5): tail then page deeper. Stable, no dup
        // across pages, respects `limit`.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let ch = RoomId::from_u128(0x5);
        for c in 0..10u64 {
            sink.append(&durable_at(ch, 1, c)).await.unwrap();
        }

        // First page of 4 from the beginning.
        let page1 = sink.page(ch, None, 4).await.unwrap();
        assert_eq!(page1.len(), 4, "limit honored");
        let c1: Vec<u64> = page1.iter().map(|e| e.seq.counter).collect();
        assert_eq!(c1, vec![0, 1, 2, 3]);

        // Page deeper from the last cursor of page1 — strictly after, no dup.
        let cursor = page1[3].cursor();
        let page2 = sink.page(ch, Some(cursor), 4).await.unwrap();
        let c2: Vec<u64> = page2.iter().map(|e| e.seq.counter).collect();
        assert_eq!(c2, vec![4, 5, 6, 7], "strictly after, no dup across pages");

        // Final page exhausts the tail.
        let cursor = page2[3].cursor();
        let page3 = sink.page(ch, Some(cursor), 4).await.unwrap();
        let c3: Vec<u64> = page3.iter().map(|e| e.seq.counter).collect();
        assert_eq!(c3, vec![8, 9], "tail shorter than limit");

        // No event appears in two pages.
        let mut all: Vec<u64> = c1.into_iter().chain(c2).chain(c3).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 10, "every event delivered exactly once");
    }

    #[tokio::test]
    async fn page_strictly_after_at_same_seq_tiebreaks_on_event_id() {
        // Two events sharing an exact (epoch, counter) page in event_id
        // order; a cursor at the lower one returns only the higher.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let ch = RoomId::from_u128(0x1234);
        let mut lo = durable_at(ch, 5, 5);
        let mut hi = durable_at(ch, 5, 5);
        lo = lo.with_event_id(EventId::from_u128(0x1));
        hi = hi.with_event_id(EventId::from_u128(0x2));
        sink.append(&lo).await.unwrap();
        sink.append(&hi).await.unwrap();

        let page = sink.page(ch, None, 10).await.unwrap();
        assert_eq!(page[0].event_id, lo.event_id);
        assert_eq!(page[1].event_id, hi.event_id);

        let after = sink.page(ch, Some(lo.cursor()), 10).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].event_id, hi.event_id);
    }

    #[tokio::test]
    async fn page_tail_returns_newest_n_ascending() {
        // Card 8428ae8c: the reverse page returns the LAST n in
        // ascending order — one DESC-indexed query, reversed in memory.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let ch = RoomId::from_u128(0x7a11);
        for c in 0..10u64 {
            sink.append(&durable_at(ch, 1, c)).await.unwrap();
        }

        let tail = sink.page_tail(ch, None, 4).await.unwrap();
        let counters: Vec<u64> = tail.iter().map(|e| e.seq.counter).collect();
        assert_eq!(counters, vec![6, 7, 8, 9], "newest 4, ascending");

        // n beyond the channel: the whole channel.
        let all = sink.page_tail(ch, None, 100).await.unwrap();
        let counters: Vec<u64> = all.iter().map(|e| e.seq.counter).collect();
        assert_eq!(counters, (0..10).collect::<Vec<u64>>());

        // Empty channel: empty tail.
        assert!(sink
            .page_tail(RoomId::from_u128(0xd0), None, 5)
            .await
            .unwrap()
            .is_empty());

        // Channel isolation: another channel's events never ride along.
        let other = RoomId::from_u128(0x7a12);
        sink.append(&durable_at(other, 1, 99)).await.unwrap();
        let tail = sink.page_tail(ch, None, 100).await.unwrap();
        assert_eq!(tail.len(), 10, "tail is per-channel");
    }

    #[tokio::test]
    async fn page_tail_strictly_before_cursor_across_epochs() {
        // The `before` bound mirrors `page`'s strictly-after predicate
        // in generational order: epoch dominates counter, event_id
        // tiebreaks — and the row AT the cursor is excluded.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let ch = RoomId::from_u128(0xe9);
        // Appended out of order on purpose; total order is
        // (1,0) (1,1) (1,2) (2,0) (2,1).
        sink.append(&durable_at(ch, 2, 1)).await.unwrap();
        sink.append(&durable_at(ch, 1, 2)).await.unwrap();
        sink.append(&durable_at(ch, 1, 0)).await.unwrap();
        sink.append(&durable_at(ch, 2, 0)).await.unwrap();
        sink.append(&durable_at(ch, 1, 1)).await.unwrap();

        let before = durable_at(ch, 2, 0).cursor();
        let tail = sink.page_tail(ch, Some(before), 2).await.unwrap();
        let order: Vec<(u64, u64)> = tail.iter().map(|e| (e.seq.epoch, e.seq.counter)).collect();
        assert_eq!(
            order,
            vec![(1, 1), (1, 2)],
            "newest 2 strictly before (2,0): the cursor row is excluded, epoch dominates"
        );

        // A cursor before everything: empty tail.
        let before = durable_at(ch, 1, 0).cursor();
        assert!(sink
            .page_tail(ch, Some(before), 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn empty_channel_pages_empty() {
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let empty = RoomId::from_u128(0xdead);
        assert!(sink.page(empty, None, 100).await.unwrap().is_empty());
        let cursor = Cursor::new(Seq::new(1, 1), EventId::from_u128(1));
        assert!(sink
            .page(empty, Some(cursor), 100)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn page_isolates_channels() {
        // A page on one channel never leaks another channel's events.
        let sink = SqliteDurableSink::in_memory().await.unwrap();
        let a = RoomId::from_u128(0xaaaa);
        let b = RoomId::from_u128(0xbbbb);
        sink.append(&durable_at(a, 1, 0)).await.unwrap();
        sink.append(&durable_at(b, 1, 0)).await.unwrap();
        sink.append(&durable_at(a, 1, 1)).await.unwrap();

        assert_eq!(sink.page(a, None, 100).await.unwrap().len(), 2);
        assert_eq!(sink.page(b, None, 100).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn persists_across_handles_on_disk() {
        // Real durability: append through one handle, reopen the file,
        // read it back — the WAL-backed row survives.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bus_events.sqlite");
        let ch = RoomId::from_u128(0x42);
        let e = durable_at(ch, 1, 0);

        SqliteDurableSink::open_path(&path)
            .await
            .unwrap()
            .append(&e)
            .await
            .unwrap();

        let reopened = SqliteDurableSink::open_path(&path).await.unwrap();
        let page = reopened.page(ch, None, 100).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0], e);
    }

    #[test]
    fn kind_and_delivery_text_round_trip_every_variant() {
        // The enum<->text mapping must cover every variant both ways so a
        // future variant can't silently corrupt the durable tier.
        for kind in [
            Kind::Message,
            Kind::Event,
            Kind::Command,
            Kind::CommandResult,
            Kind::Signal,
            Kind::StreamChunk,
            Kind::Control,
        ] {
            assert_eq!(kind_from_text(kind_to_text(kind)).unwrap(), kind);
        }
        for delivery in [
            DeliveryClass::Durable,
            DeliveryClass::EphemeralLatest,
            DeliveryClass::EphemeralWindow,
            DeliveryClass::RequestResponse,
            DeliveryClass::StreamChunk,
        ] {
            assert_eq!(
                delivery_from_text(delivery_to_text(delivery)).unwrap(),
                delivery
            );
        }
    }
}
