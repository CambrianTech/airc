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

/// The store's one busy and pool-acquire backstop (`crate::sqlite::POOL_ACQUIRE_TIMEOUT`).
use crate::sqlite::POOL_ACQUIRE_TIMEOUT;

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
    /// The file's write gate, shared by every handle on it in this process ([`crate::write_gate`]).
    gate: std::sync::Arc<crate::write_gate::FileGate>,
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
        Self::open_gated(db_url, admission, crate::write_gate::unshared()).await
    }

    async fn open_gated(
        db_url: &str,
        admission: crate::custody::Admission,
        gate: std::sync::Arc<crate::write_gate::FileGate>,
    ) -> Result<Self, BusError> {
        let custody = crate::custody::WriteGate::new(admission).map_err(BusError::sink_source)?;
        let mut opts = Self::pool_options(db_url);
        // WAL + the busy timeout ride the connection options so the journal switch
        // never needs a separate exclusive statement racing another opener — see
        // `SqliteEventStore::open` (2026-09-14: "database is locked" at attach on
        // Windows CI). The driver applies them to every connection it opens, an
        // old DB's included (§3.3: WAL lets deep-replay reads run beside the one
        // writer; `synchronous=NORMAL` fsyncs per checkpoint, not per commit).
        opts.map_sqlx_sqlite_opts(crate::sqlite::file_options);
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
            // The writer's FILE, not its pragmas: `auto_vacuum` (`sqlite::file_options`) is a
            // write a read-only connection refuses. Only reads use this pool; migrations run once
            // above.
            let read_options = read_only_options(sqlite_options.get_filename());
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
            gate,
        })
    }

    /// Admit one write: the file's turn ([`crate::write_gate`]), custody, then no migration past
    /// this binary since open ([`crate::SqliteEventStore::admit_write`] is the same rule).
    async fn admit_write(&self) -> Result<crate::write_gate::Write<'_>, BusError> {
        let queued = std::time::Instant::now();
        let epoch = self.gate.epoch_now();
        let turn = self.gate.enter().await;
        let entered = std::time::Instant::now();
        let ticket = self.custody.admit().map_err(BusError::sink_source)?;
        crate::migration::refuse_if_ahead(&self.db)
            .await
            .map_err(BusError::sink_source)?;
        Ok(crate::write_gate::Write::new(
            &self.gate, queued, entered, epoch, true, turn, ticket,
        ))
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
        Self::open_gated(
            &sqlite_file_url(path),
            admission,
            crate::write_gate::for_path(path),
        )
        .await
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
        opts.map_sqlx_sqlite_opts(move |_| read_only_options(&path));
        let db = Database::connect(opts)
            .await
            .map_err(BusError::sink_source)?;
        // Read-only: SQLite refuses its writes, so they need no admission.
        Ok(Self {
            readers: db.clone(),
            db,
            custody: crate::custody::WriteGate::new(crate::custody::Admission::Unmanaged)
                .map_err(BusError::sink_source)?,
            gate: crate::write_gate::unshared(),
        })
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

        let read = || async {
            let mut branches = Vec::new();
            for query in page_rows(channel, after, through, rule, bounded, tail) {
                branches.push(
                    query
                        .all(&self.readers)
                        .await
                        .map_err(BusError::sink_source)?,
                );
            }
            crate::event_table::merge_branches(
                branches,
                |row| (row.epoch, row.counter, row.event_id),
                limit,
                tail,
            )
            .into_iter()
            .map(from_model)
            .collect::<Result<Vec<_>, BusError>>()
        };
        // A predicate on derived data first derives the owed rows that could belong to its page,
        // nearest first; a plain page derives nothing (crate::derive).
        if !crate::derive::reads_derived(rule.headers, rule.addressed_to.is_some(), false) {
            return read().await;
        }
        crate::derive::settle_page::<bus_event::Entity, _, _, BusError, _, _, _, _>(
            &self.db,
            in_range(channel, after, through),
            tail,
            limit,
            || self.admit_write(),
            BusError::sink_source,
            read,
            |far: &Envelope| {
                if tail {
                    strictly_before(far.cursor()).not()
                } else {
                    strictly_after(far.cursor()).not()
                }
            },
        )
        .await
    }

    async fn count_matching(
        &self,
        channel: RoomId,
        after: Cursor,
        through: Cursor,
        headers: &airc_core::HeaderFilter,
        exclude: Option<&airc_core::RuntimeSelf>,
    ) -> Result<airc_core::HistoryCount, BusError> {
        // A count derives nothing: rows in its range that still owe their header rows make it a
        // lower bound (`more`), never a wait on a derivation (crate::derive).
        let owed = crate::derive::reads_derived(headers, false, exclude.is_some())
            && crate::derive::owes::<bus_event::Entity>(
                &self.readers,
                in_range(channel, after, through),
            )
            .await
            .map_err(BusError::sink_source)?;
        let mut rows = matching_rows(channel, after, through, headers);
        if owed {
            // An owed row has no header rows yet, so a negated filter would count it falsely:
            // counting derived rows only keeps the count a lower bound.
            rows =
                rows.filter(bus_event::Column::DerivedVersion.eq(crate::derive::DERIVED_VERSION));
        }
        if let Some(rule) = exclude {
            rows = rows.filter(crate::header_query::not_runtime_self::<bus_event::Entity>(
                rule,
            ));
        }
        // Newest first, at most one past the cap: the count never walks the whole range, and
        // no header JSON or payload leaves SQLite.
        let times: Vec<i64> = rows
            .select_only()
            .column(bus_event::Column::OccurredAtMs)
            .order_by_desc(bus_event::Column::Epoch)
            .order_by_desc(bus_event::Column::Counter)
            .order_by_desc(bus_event::Column::EventId)
            .limit(airc_core::HISTORY_COUNT_CAP + 1)
            .into_tuple()
            .all(&self.readers)
            .await
            .map_err(BusError::sink_source)?;
        let times = times
            .into_iter()
            .map(|ms| i64_to_u64("occurred_at_ms", ms))
            .collect::<Result<Vec<_>, _>>()?;
        let mut count = airc_core::HistoryCount::of_newest(times);
        count.more |= owed;
        Ok(count)
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
            .all(&self.readers)
            .await
            .map_err(BusError::sink_source)?;

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

        if let Some(c) = before {
            query = query.filter(strictly_before(c));
        }

        let rows = query
            .order_by_desc(bus_event::Column::Epoch)
            .order_by_desc(bus_event::Column::Counter)
            .order_by_desc(bus_event::Column::EventId)
            .limit(bounded)
            .all(&self.readers)
            .await
            .map_err(BusError::sink_source)?;

        // DESC off the index, ascending to the caller.
        rows.into_iter().rev().map(from_model).collect()
    }

    /// Continuum #297: `page_tail` with the kind predicate applied by
    /// the DATABASE, before `LIMIT` — the store leg of "newest N events
    /// of these kinds". Same composite-index DESC scan as
    /// [`Self::page_tail`] plus one indexed-column `IN` predicate; a
    /// chunk-flooded room answers with the newest N matching rows
    /// instead of N chunks the caller would throw away.
    async fn page_tail_of_kinds(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        let bounded = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .min(i64::MAX as u64);

        let mut query = bus_event::Entity::find()
            .filter(bus_event::Column::RoomId.eq(channel.as_uuid()))
            .filter(bus_event::Column::Kind.is_in(kinds.iter().map(|k| kind_to_text(*k))));

        if let Some(c) = before {
            query = query.filter(strictly_before(c));
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
        leading_mention: Set(addressing.leading_mention),
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
/// The durable rows of `channel` in `(after, through]`: one owner for the range `page_matching`
/// pages, `count_matching` counts and a derivation of either scopes to.
fn in_range(channel: RoomId, after: Cursor, through: Cursor) -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(bus_event::Column::RoomId.eq(channel.as_uuid()))
        .add(strictly_after(after))
        .add(strictly_after(through).not())
        .add(bus_event::Column::Delivery.eq("durable"))
}

/// Strictly after `c` in generational order:
///   epoch > c.epoch
///   OR (epoch == c.epoch AND counter > c.counter)
///   OR (epoch == c.epoch AND counter == c.counter AND event_id > c.event_id).
/// event_id is the deterministic tiebreaker (§3.5); within one epoch the monotonic counter
/// forbids a tie, but the tiebreak keeps the order total independent of how `seq` was produced.
fn strictly_after(c: Cursor) -> sea_orm::sea_query::SimpleExpr {
    let epoch = c.seq.epoch as i64;
    let counter = c.seq.counter as i64;
    let event_id = c.event_id.as_uuid();
    Expr::col(bus_event::Column::Epoch)
        .gt(epoch)
        .or(Expr::col(bus_event::Column::Epoch)
            .eq(epoch)
            .and(Expr::col(bus_event::Column::Counter).gt(counter)))
        .or(Expr::col(bus_event::Column::Epoch)
            .eq(epoch)
            .and(Expr::col(bus_event::Column::Counter).eq(counter))
            .and(Expr::col(bus_event::Column::EventId).gt(event_id)))
}

/// [`in_range`]'s rows whose headers match.
fn matching_rows(
    channel: RoomId,
    after: Cursor,
    through: Cursor,
    headers: &airc_core::HeaderFilter,
) -> sea_orm::Select<bus_event::Entity> {
    bus_event::Entity::find()
        .filter(in_range(channel, after, through))
        .filter(crate::header_query::predicate::<bus_event::Entity>(headers))
}

/// The selects `page_matching` reads: [`matching_rows`], the first `limit` in
/// cursor order (the last, for `tail`); with an addressee filter, one per
/// branch ([`crate::event_table::addressee_branches`]).
fn page_rows(
    channel: RoomId,
    after: Cursor,
    through: Cursor,
    rule: airc_core::HistoryMatch<'_>,
    limit: u64,
    tail: bool,
) -> Vec<sea_orm::Select<bus_event::Entity>> {
    let order = if tail {
        sea_orm::Order::Desc
    } else {
        sea_orm::Order::Asc
    };
    let query = matching_rows(channel, after, through, rule.headers)
        .order_by(bus_event::Column::Epoch, order.clone())
        .order_by(bus_event::Column::Counter, order.clone())
        .order_by(bus_event::Column::EventId, order)
        .limit(limit);
    crate::event_table::addressee_branches(query, rule.addressed_to)
}

/// A read-only connection to the store file at `path`: the typed filename (`?` and `#` in a path
/// never become connection parameters), never created, waiting as long as the pool does, and no
/// pragma that writes. The one shape for the sink's reader pool and `open_read_only_path`.
fn read_only_options(path: &Path) -> sea_orm::sqlx::sqlite::SqliteConnectOptions {
    sea_orm::sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false)
        .busy_timeout(POOL_ACQUIRE_TIMEOUT)
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
mod tests;
