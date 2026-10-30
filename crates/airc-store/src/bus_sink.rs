//! SQLite-backed [`airc_bus::DurableSink`] — the real ORM durable tier
//! (§3.3 of `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! This replaces the in-memory test sink (`airc_bus::InMemoryDurableSink`)
//! with a SeaORM-backed SQLite store so the owner-core's `Durable`
//! envelopes persist and replay from disk. The owner daemon is the
//! **single writer** of this durable tier (§3.3). File-backed stores use
//! **WAL** and separate, bounded reader and writer pools with private caches
//! so deep-replay cannot consume the writer's connection. Other handles/
//! processes can still contend for SQLite's write lock.
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
    QueryOrder, QuerySelect, Set, TransactionTrait,
};

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind, Target};
use airc_bus::{BusError, DurableSink, Seq};
use airc_core::{ClientId, EventId, Headers, PeerId, RoomId};

use crate::entities::bus_event;

/// Deadlock backstop for the single-connection pool — NOT a load limit.
///
/// These pools are `max_connections(1)`: concurrent callers queue by
/// design, so a caller waiting is normal operation, not a fault. At the
/// previous 5s this timeout behaved as a load limiter and turned routine
/// queuing into a hard error — it took down `peer_add_with_tier_flag_
/// persists_explicit_tier` on windows CI (2026-08-12) with "pool timed
/// out while waiting for an open connection" during `airc init`, a
/// failure that has nothing to do with the code under test. A backstop
/// should only fire when something is genuinely stuck, so it is sized
/// for the slowest legitimate wait (loaded CI, cold Windows I/O),
/// leaving real deadlocks still bounded.
const POOL_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// SQLite-backed durable tier for the owner-core (§3.3).
///
/// Ordinary file-backed WAL stores reserve one connection for persistence
/// and one for reads. Memory/temporary databases, SQLite URI filenames,
/// shared caches, and read-only observers retain a single pool.
pub struct SqliteDurableSink {
    db: DatabaseConnection,
    readers: DatabaseConnection,
    /// How this sink's writes are admitted ([`crate::custody`]).
    custody: crate::custody::WriteGate,
}

impl SqliteDurableSink {
    fn pool_options(db_url: &str) -> ConnectOptions {
        let mut opts = ConnectOptions::new(db_url.to_owned());
        opts.connect_timeout(POOL_ACQUIRE_TIMEOUT)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .max_connections(1);
        opts
    }

    /// Open (or create) a SQLite database at `db_url`, run migrations,
    /// and put the connection in WAL mode.
    ///
    /// For an in-memory store use `"sqlite::memory:"`; for a file store
    /// use `"sqlite://<path>?mode=rwc"`.
    pub(crate) async fn open(
        db_url: &str,
        admission: crate::custody::Admission,
    ) -> Result<Self, BusError> {
        let custody = crate::custody::WriteGate::new(admission).map_err(BusError::sink_source)?;
        let mut opts = Self::pool_options(db_url);
        // WAL + the busy timeout ride the connection options so the journal switch
        // never needs a separate exclusive statement racing another opener — see
        // `SqliteEventStore::open` (2026-09-14: "database is locked" at attach on
        // Windows CI). The driver applies them to every connection it opens, an
        // old DB's included (§3.3: WAL lets deep-replay reads run beside the one
        // writer; `synchronous=NORMAL` fsyncs per checkpoint, not per commit).
        opts.map_sqlx_sqlite_opts(|so| {
            use sea_orm::sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};
            so.journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal)
                .busy_timeout(POOL_ACQUIRE_TIMEOUT)
        });
        let write = custody.admit().map_err(BusError::sink_source)?;
        let db = Database::connect(opts)
            .await
            .map_err(BusError::sink_source)?;
        // Migrate forward atomically, derive what rows owe, and refuse a DB
        // migrated AHEAD of this binary (`StoreError::AheadOfBinary`).
        crate::migration::apply_migrations(&db)
            .await
            .map_err(BusError::sink_source)?;
        // Inspect the driver's resolved mode rather than guessing from the
        // input URL. Reopening a memory URL can create a different database;
        // shared-cache memory also lacks file-WAL reader/writer isolation.
        use sea_orm::sqlx::ConnectOptions as _;
        let sqlite_options = db.get_sqlite_connection_pool().connect_options();
        // SQLx's URL serializer can reject its own generated memory filename
        // as a URL authority. A classification-only clone keeps the typed mode
        // but replaces the filename; actual connections retain it unchanged.
        let shared_pool_mode = sqlite_options
            .as_ref()
            .clone()
            .filename("airc-mode-probe.sqlite")
            .to_url_lossy()
            .query_pairs()
            .any(|(key, value)| {
                (key == "mode" && value == "memory") || (key == "cache" && value == "shared")
            });
        // An empty SQLite filename denotes a connection-local temporary DB.
        // SQLite also interprets `file:` filenames as URIs, including memory
        // aliases not represented by SQLx's mode flag. Conservatively keep
        // those on the existing pool rather than reinterpret their identity.
        // Explicit shared caches also retain serialization: their table locks
        // can block writes behind reads even when the file uses WAL.
        let share_pool = shared_pool_mode
            || sqlite_options.get_filename().as_os_str().is_empty()
            || sqlite_options.get_filename() == Path::new(":memory:")
            || sqlite_options
                .get_filename()
                .to_string_lossy()
                .starts_with("file:");
        let readers = if share_pool {
            db.clone()
        } else {
            // Preserve the exact filename/VFS/cache options of the opened
            // writer. Only reads use this pool; migrations run once above.
            let read_options = sqlite_options
                .as_ref()
                .clone()
                .read_only(true)
                .create_if_missing(false);
            let mut opts = Self::pool_options(db_url);
            opts.map_sqlx_sqlite_opts(move |_| read_options.clone());
            Database::connect(opts)
                .await
                .map_err(BusError::sink_source)?
        };
        drop(write);
        Ok(Self {
            db,
            readers,
            custody,
        })
    }

    /// Admit one write: custody, then no migration past this binary since
    /// open ([`crate::SqliteEventStore::admit_write`] is the same rule).
    async fn admit_write(&self) -> Result<crate::custody::WriteTicket<'_>, BusError> {
        let ticket = self.custody.admit().map_err(BusError::sink_source)?;
        crate::migration::refuse_if_ahead(&self.db)
            .await
            .map_err(BusError::sink_source)?;
        Ok(ticket)
    }

    /// Open a file-backed durable tier from a filesystem path. Keeps
    /// platform path rules (Windows URI slashes) out of consumers.
    pub async fn open_path(path: &Path) -> Result<Self, BusError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(BusError::sink_source)?;
            }
        }
        let admission = crate::custody::resolve(path).map_err(BusError::sink_source)?;
        Self::open(&sqlite_file_url(path), admission).await
    }

    /// Inspect an existing owner store without creating it, migrating it, or
    /// changing journal mode. Ordinary reads see the live WAL; this is not an
    /// immutable snapshot. Missing files/schema and read failures stay errors.
    pub async fn open_read_only_path(path: &Path) -> Result<Self, BusError> {
        let path = path.to_owned();
        let mut opts = Self::pool_options("sqlite::memory:");
        opts.sqlx_logging(false);
        // Supply the native filename through the typed driver, not a URI: `?`
        // and `#` in a path must not become connection parameters.
        opts.map_sqlx_sqlite_opts(move |_| {
            sea_orm::sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .read_only(true)
                .create_if_missing(false)
                .busy_timeout(POOL_ACQUIRE_TIMEOUT)
        });
        let db = Database::connect(opts)
            .await
            .map_err(BusError::sink_source)?;
        // Read-only: SQLite refuses its writes, so they need no admission.
        Ok(Self {
            readers: db.clone(),
            db,
            custody: crate::custody::WriteGate::new(crate::custody::Admission::Unmanaged)
                .map_err(BusError::sink_source)?,
        })
    }

    /// Read-only candidate admission. An incompatible populated store is a
    /// typed refusal before the installer displaces the currently running build.
    pub async fn check_classification_adoption(&self) -> Result<(), crate::StoreError> {
        crate::migration::check_classification_adoption(&self.readers).await
    }

    /// Retrieve original pre-classification records without migrations or
    /// interpretation. Open with `open_read_only_path` to inspect a refused
    /// store. Cursor is the last returned event ID; the requested limit is a
    /// required nonzero bound. This never manufactures routable envelopes.
    pub async fn audit_page(
        &self,
        table: crate::audit::AuditTable,
        after: Option<uuid::Uuid>,
        limit: std::num::NonZeroU32,
    ) -> Result<Vec<crate::audit::AuditRecord>, crate::StoreError> {
        crate::audit::page(&self.readers, table, after, limit).await
    }

    /// Open an ephemeral in-memory durable tier. Convenience for tests.
    pub async fn in_memory() -> Result<Self, BusError> {
        Self::open("sqlite::memory:", crate::custody::Admission::Unmanaged).await
    }

    /// Bump the persisted generational epoch on **this** ORM and return
    /// the [`SqliteEpochStore`] capturing the new value. Run once per
    /// daemon start. Sharing the sink's connection keeps the durable
    /// transcript (`bus_event`) and the epoch cell (`bus_epoch`) in one
    /// ORM — the single-writer machine store (§3.3 / §3.8).
    pub async fn bump_epoch(&self) -> Result<crate::SqliteEpochStore, BusError> {
        let _write = self.admit_write().await?;
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
        self.append_batch(&[e]).await
    }

    /// Group-commit: one multi-row INSERT … ON CONFLICT DO NOTHING for the
    /// events and their header rows, in ONE transaction (one commit per batch,
    /// not per event); fsync work is amortized at WAL checkpoint under
    /// synchronous=NORMAL. Same idempotent-on-`event_id` contract: a replayed
    /// event and its headers conflict and are skipped. On error nothing is
    /// committed and the caller re-pins the whole batch (no partial-persist
    /// ambiguity — verified by `append_batch_mixed_persists_every_new_event`).
    async fn append_batch(&self, events: &[&Envelope]) -> Result<(), BusError> {
        if events.is_empty() {
            return Ok(());
        }
        let sink = |err: sea_orm::DbErr| BusError::Sink(err.to_string());
        let _write = self.admit_write().await?;
        let actives = events
            .iter()
            .map(|e| to_active_model(e))
            .collect::<Result<Vec<_>, _>>()?;
        let txn = self.db.begin().await.map_err(sink)?;
        match bus_event::Entity::insert_many(actives)
            .on_conflict(
                OnConflict::column(bus_event::Column::EventId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&txn)
            .await
        {
            // Every row in the batch already existed (all conflicted) — the
            // DO-NOTHING all-dupes path. Idempotent success, not an error.
            Ok(_) | Err(sea_orm::DbErr::RecordNotInserted) => {}
            Err(err) => return Err(sink(err)),
        }
        crate::header_query::write_header_rows::<bus_event::Entity>(
            &txn,
            events.iter().map(|e| (e.event_id.as_uuid(), &e.headers)),
        )
        .await
        .map_err(sink)?;
        txn.commit().await.map_err(sink)
    }

    async fn page_matching(
        &self,
        channel: RoomId,
        after: Cursor,
        through: Cursor,
        rule: airc_core::HistoryMatch<'_>,
        limit: usize,
        tail: bool,
    ) -> Result<Vec<Envelope>, BusError> {
        // Bounded by `limit`. The trait caller (the router's deep-replay)
        // may pass a very large limit to mean "the whole tail"; clamp to
        // `i64::MAX` so the SQL bind never overflows and we never use
        // `usize::MAX` as a sentinel. The query stays indexed: ORDER BY
        // rides the composite index, LIMIT only caps the row count.
        let bounded = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .min(i64::MAX as u64);

        let mut rows = page_rows(channel, after, through, rule, bounded, tail)?
            .all(&self.readers)
            .await
            .map_err(BusError::sink_source)?;
        if tail {
            rows.reverse();
        }
        rows.into_iter().map(from_model).collect()
    }

    async fn count_matching(
        &self,
        channel: RoomId,
        after: Cursor,
        through: Cursor,
        rule: airc_core::HistoryMatch<'_>,
        exclude: Option<&airc_core::RuntimeSelf>,
    ) -> Result<airc_core::HistoryCount, BusError> {
        let mut rows = matching_rows(channel, after, through, rule)?;
        if let Some(rule) = exclude {
            rows = rows.filter(crate::header_query::not_runtime_self::<bus_event::Entity>(
                rule,
            ));
        }
        // COUNT/MIN/MAX in the store: no row, header JSON or payload leaves SQLite.
        let row: Option<(i64, Option<i64>, Option<i64>)> = rows
            .select_only()
            .column_as(Expr::col(bus_event::Column::EventId).count(), "count")
            .column_as(Expr::col(bus_event::Column::OccurredAtMs).min(), "oldest")
            .column_as(Expr::col(bus_event::Column::OccurredAtMs).max(), "newest")
            .into_tuple()
            .one(&self.readers)
            .await
            .map_err(BusError::sink_source)?;
        let Some((count, oldest, newest)) = row else {
            return Ok(airc_core::HistoryCount::default());
        };
        Ok(airc_core::HistoryCount {
            count: i64_to_u64("count", count)?,
            oldest_ms: oldest
                .map(|ms| i64_to_u64("occurred_at_ms", ms))
                .transpose()?,
            newest_ms: newest
                .map(|ms| i64_to_u64("occurred_at_ms", ms))
                .transpose()?,
        })
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

        let mut query = bus_event::Entity::find()
            .filter(bus_event::Column::Class.is_not_null())
            .filter(bus_event::Column::RoomId.eq(channel.as_uuid()));

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
            .all(&self.readers)
            .await
            .map_err(BusError::sink_source)?;

        rows.into_iter().map(from_model).collect()
    }

    async fn page_tail(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.filtered_tail_before(channel, before, limit, &airc_bus::Filter::channel(channel))
            .await
    }

    async fn page_tail_of_kinds(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.filtered_tail_before(
            channel,
            before,
            limit,
            &airc_bus::Filter::channel(channel).with_kinds(kinds.to_vec()),
        )
        .await
    }

    /// Every live subscription axis is evaluated by the store before LIMIT.
    async fn filtered_tail_before(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
        filter: &airc_bus::Filter,
    ) -> Result<Vec<Envelope>, BusError> {
        let bounded = u64::try_from(limit)
            .map_err(BusError::sink_source)?
            .min(i64::MAX as u64);
        let mut query = bus_event::Entity::find()
            .filter(bus_event::Column::RoomId.eq(channel.as_uuid()))
            .filter(bus_event::Column::RoomId.eq(filter.channel.as_uuid()))
            .filter(
                crate::class_query::selection::<bus_event::Entity>(
                    &filter.selection,
                    &filter.headers,
                )
                .map_err(BusError::sink_source)?,
            );
        if let Some(kinds) = &filter.kinds {
            query = query
                .filter(bus_event::Column::Kind.is_in(kinds.iter().copied().map(kind_to_text)));
        }
        if let Some(delivery) = &filter.delivery {
            query = query.filter(
                bus_event::Column::Delivery.is_in(delivery.iter().copied().map(delivery_to_text)),
            );
        }
        if let Some(cursor) = before {
            query = query.filter(strictly_before(cursor));
        }
        let rows = query
            .order_by_desc(bus_event::Column::Epoch)
            .order_by_desc(bus_event::Column::Counter)
            .order_by_desc(bus_event::Column::EventId)
            .limit(bounded)
            .all(&self.readers)
            .await
            .map_err(BusError::sink_source)?;
        rows.into_iter().rev().map(from_model).collect()
    }

    /// Card 7d5b6a65: efficient head-cursor query for the
    /// `AttachRequest::from_now` path. The trait's default impl pages
    /// the entire channel to find the last cursor; the SQLite override
    /// picks the single row at the top of total order via the existing
    /// composite index — bounded cost regardless of channel depth.
    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>, BusError> {
        let row = bus_event::Entity::find()
            .filter(bus_event::Column::Class.is_not_null())
            .filter(bus_event::Column::RoomId.eq(channel.as_uuid()))
            .order_by_desc(bus_event::Column::Epoch)
            .order_by_desc(bus_event::Column::Counter)
            .order_by_desc(bus_event::Column::EventId)
            .one(&self.readers)
            .await
            .map_err(BusError::sink_source)?;
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
            .one(&self.readers)
            .await
            .map_err(BusError::sink_source)?;
        Ok(row.is_some())
    }
}

/// The ONE sanctioned `Envelope -> row` serialize (§3.3). Lives only
/// here; no other copy of this mapping exists.
fn to_active_model(e: &Envelope) -> Result<bus_event::ActiveModel, BusError> {
    let addressing = e.addressing();
    Ok(bus_event::ActiveModel {
        event_id: Set(e.event_id.as_uuid()),
        room_id: Set(e.channel.as_uuid()),
        epoch: Set(u64_to_i64("bus_events.epoch", e.seq.epoch)?),
        counter: Set(u64_to_i64("bus_events.counter", e.seq.counter)?),
        kind: Set(kind_to_text(e.kind).to_owned()),
        class: Set(Some(serde_json::to_string(&e.class).map_err(codec)?)),
        class_family: Set(Some(e.class.family() as i32)),
        delivery: Set(delivery_to_text(e.delivery).to_owned()),
        target: Set(serde_json::to_value(&e.target).map_err(codec)?),
        correlation_id: Set(e.correlation_id),
        coalesce_key: Set(e.coalesce_key.clone()),
        headers: Set(serde_json::to_value(&e.headers).map_err(codec)?),
        payload: Set(e.payload.to_vec()),
        peer_id: Set(e.from.0.as_uuid()),
        client_id: Set(e.from.1.as_uuid()),
        occurred_at_ms: Set(u64_to_i64("bus_events.occurred_at_ms", e.occurred_at_ms)?),
        addressed_peer: Set(addressing.peer.map(|peer| peer.as_uuid())),
        leading_mention: Set(false),
        derived_version: Set(crate::derive::DERIVED_VERSION),
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
        class: crate::class_query::decode(m.event_id, m.class, m.class_family)
            .map_err(BusError::sink_source)?,
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

/// Strictly-before-`c` in the generational total order — the exact
/// mirror of `page`'s strictly-after predicate:
///   epoch < c.epoch
///   OR (epoch == c.epoch AND counter < c.counter)
///   OR (epoch == c.epoch AND counter == c.counter
///       AND event_id < c.event_id).
/// The durable rows of `channel` in `(after, through]` whose headers match: one
/// owner for the range `page_matching` pages and `count_matching` counts.
fn matching_rows(
    channel: RoomId,
    after: Cursor,
    through: Cursor,
    rule: airc_core::HistoryMatch<'_>,
) -> Result<sea_orm::Select<bus_event::Entity>, BusError> {
    let mut query = bus_event::Entity::find()
        .filter(bus_event::Column::Class.is_not_null())
        .filter(bus_event::Column::RoomId.eq(channel.as_uuid()));

    // Strictly after the cursor in generational order:
    //   epoch > c.epoch
    //   OR (epoch == c.epoch AND counter > c.counter)
    //   OR (epoch == c.epoch AND counter == c.counter
    //       AND event_id > c.event_id).
    // event_id is the deterministic tiebreaker (§3.5); within one
    // epoch the monotonic counter forbids a tie, but the tiebreak
    // keeps the order total independent of how `seq` was produced.
    {
        let c = after;
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

    Ok(query
        .filter(
            strictly_before(through)
                .or(Expr::col(bus_event::Column::EventId).eq(through.event_id.as_uuid())),
        )
        .filter(
            crate::class_query::matching::<bus_event::Entity>(rule)
                .map_err(BusError::sink_source)?,
        )
        .filter(bus_event::Column::Delivery.eq("durable")))
}

/// The selects `page_matching` reads: [`matching_rows`], the first `limit` in
/// cursor order (the last, for `tail`). All predicates run before LIMIT.
fn page_rows(
    channel: RoomId,
    after: Cursor,
    through: Cursor,
    rule: airc_core::HistoryMatch<'_>,
    limit: u64,
    tail: bool,
) -> Result<sea_orm::Select<bus_event::Entity>, BusError> {
    let order = if tail {
        sea_orm::Order::Desc
    } else {
        sea_orm::Order::Asc
    };
    let query = matching_rows(channel, after, through, rule)?
        .order_by(bus_event::Column::Epoch, order.clone())
        .order_by(bus_event::Column::Counter, order.clone())
        .order_by(bus_event::Column::EventId, order)
        .limit(limit);
    Ok(query)
}

/// Shared by both reverse-paging legs ([`DurableSink::page_tail`],
/// [`DurableSink::page_tail_of_kinds`]) so the ordering contract has
/// exactly one home.
fn strictly_before(c: Cursor) -> sea_orm::sea_query::SimpleExpr {
    let epoch = c.seq.epoch as i64;
    let counter = c.seq.counter as i64;
    let event_id = c.event_id.as_uuid();
    Expr::col(bus_event::Column::Epoch)
        .lt(epoch)
        .or(Expr::col(bus_event::Column::Epoch)
            .eq(epoch)
            .and(Expr::col(bus_event::Column::Counter).lt(counter)))
        .or(Expr::col(bus_event::Column::Epoch)
            .eq(epoch)
            .and(Expr::col(bus_event::Column::Counter).eq(counter))
            .and(Expr::col(bus_event::Column::EventId).lt(event_id)))
}

pub(crate) fn kind_to_text(kind: Kind) -> &'static str {
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
mod tests;
