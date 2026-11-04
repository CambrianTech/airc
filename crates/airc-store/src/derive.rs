//! Data the store derives from each stored event: the addressing columns
//! ([`airc_core::Addressing`]) and the header rows (`crate::header_query`) of
//! `events` and `bus_events`.
//!
//! A row is current when its `derived_version` is [`DERIVED_VERSION`]. Appends
//! by this binary write it current. A row at an older version (stored before
//! migration 000021, or inserted by an older writer without the column) is
//! derived by the store's background task, a batch at a time: the batch is
//! found without the write gate, then written in one transaction under it (its
//! header rows replaced, its addressing recomputed, its version set), so a
//! half-derived row is never visible and no scan holds the gate.
//!
//! Nothing derives at open or in a read (card 70055c76: open derived the whole
//! table, 23.4 s on a 1.56 GB store; a 50-row inbox read that derived its own
//! range took 99.8 s before the first pass). Instead:
//! - a read on derived data ([`reads_derived`]) matches derived rows only
//!   ([`derived`]) and reports its file's settlement (`crate::settling`):
//!   unsettled until the file is derived and indexed, so a missing old match is
//!   never silent;
//! - the background task's first step, at open, derives the owed rows retention
//!   keeps, newest first, then settles the file ([`SqliteEventStore::settle`]);
//! - each pass derives what remains after its drain (`crate::retention::run::pass`).

use std::collections::BTreeMap;

use airc_core::HeaderFilter;
use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait, Order, QueryFilter,
    QueryOrder, QuerySelect, TransactionTrait,
};
use uuid::Uuid;

use crate::entities::{bus_event, event};
use crate::error::StoreError;
use crate::event_table::EventTable;
use crate::header_query::write_header_rows;
use crate::migration::fault::{self, Fault};
use crate::sqlite::SqliteEventStore;
use crate::write_gate::FileGate;

/// The version of the derived data this binary writes.
pub(crate) const DERIVED_VERSION: i32 = 1;

/// Rows derived per transaction.
const BATCH: u64 = 1000;

/// What the drain deletes from a table, judged from a row's own columns, so it holds for a row
/// that owes its derivation (no header rows yet). One owner: `crate::retention`. The derivation
/// at open skips these: the next pass deletes them, so no read can ever need them.
pub(crate) trait Dropped {
    fn dropped() -> Option<SimpleExpr>;
}

/// `scope` without the rows the drain deletes.
fn kept<T: Dropped>(scope: Condition) -> Condition {
    match T::dropped() {
        Some(dropped) => scope.add(dropped.not()),
        None => scope,
    }
}

/// Whether a read's predicate reads derived data: a header predicate and the exclusion of a
/// runtime's own sends read the header rows, an addressee filter the addressing columns.
pub(crate) fn reads_derived(headers: &HeaderFilter, addressee: bool, exclude: bool) -> bool {
    !matches!(headers, HeaderFilter::Any) || addressee || exclude
}

/// Rows of `T` whose derived data is current: what a read on derived data may match, so an owed
/// row (no header rows yet) never matches a negated filter falsely.
pub(crate) fn derived<T: EventTable>() -> SimpleExpr {
    Expr::col(T::DERIVED_VERSION).eq(DERIVED_VERSION)
}

/// At open: a file with no events owes nothing, so its indexes are built (on empty tables, at
/// once) and it is settled now (a new store, a test's in-memory one). Any other file is indexed
/// if its indexes already exist (a store migrated before 000021 stopped building them), and
/// derived when the background task says so. Two one-row reads and six catalog lookups.
pub(crate) async fn settle_at_open(
    db: &DatabaseConnection,
    gate: &FileGate,
) -> Result<(), StoreError> {
    if is_empty::<event::Entity>(db).await? && is_empty::<bus_event::Entity>(db).await? {
        crate::indexes::ensure(db).await?;
        gate.settling.mark_indexed();
        gate.settling.mark_derived();
    } else if crate::indexes::all_exist(db).await? {
        gate.settling.mark_indexed();
    }
    Ok(())
}

async fn is_empty<T: EventTable>(db: &DatabaseConnection) -> Result<bool, StoreError> {
    let any: Option<Uuid> = T::find()
        .select_only()
        .column(T::EVENT_ID)
        .limit(1)
        .into_tuple()
        .one(db)
        .await?;
    Ok(any.is_none())
}

impl SqliteEventStore {
    /// Derive the owed rows retention keeps in both event tables, newest first, then mark the file
    /// derived (`crate::settling`): the background task's first step, at open. How many rows.
    pub async fn settle(&self) -> Result<usize, StoreError> {
        let derived = self
            .derive_owed::<event::Entity>(kept::<event::Entity>(Condition::all()))
            .await?
            + self
                .derive_owed::<bus_event::Entity>(kept::<bus_event::Entity>(Condition::all()))
                .await?;
        self.gate().settling.mark_derived();
        Ok(derived)
    }

    /// The file's settlement, live: it moves to Settled once, when the file is both derived and
    /// indexed (`crate::settling`), so a reader holding an unsettled answer re-reads on it rather
    /// than polling.
    pub fn settled(&self) -> tokio::sync::watch::Receiver<airc_core::Settlement> {
        self.gate().settling.subscribe()
    }

    /// Derive every owed row of both event tables: each pass's step after its drain. How many.
    pub async fn derive_all_owed(&self) -> Result<usize, StoreError> {
        let derived = self.derive_owed::<event::Entity>(Condition::all()).await?
            + self
                .derive_owed::<bus_event::Entity>(Condition::all())
                .await?;
        self.gate().settling.mark_derived();
        Ok(derived)
    }

    /// Derive every owed row of `T` within `scope`, newest first: each batch found without the
    /// gate, written under it.
    async fn derive_owed<T: EventTable>(&self, scope: Condition) -> Result<usize, StoreError>
    where
        <T::Headers as EntityTrait>::ActiveModel: Send,
    {
        let mut derived = 0;
        loop {
            let batch = owed_batch::<T>(self.connection(), scope.clone()).await?;
            if batch.is_empty() {
                return Ok(derived);
            }
            let write = self.admit_write().await?;
            derived += write_batch::<T>(self.connection(), batch).await?;
            drop(write);
            fault::hit(Fault::AfterDerivedBatch)?;
        }
    }
}

/// A batch's non-default addressing, grouped by value: the rows whose typed
/// target names each peer, and the rows that open with an `@name`.
#[derive(Default)]
struct Grouped {
    peers: BTreeMap<Uuid, Vec<Uuid>>,
    leading: Vec<Uuid>,
}

impl Grouped {
    fn add(&mut self, id: Uuid, addressing: airc_core::Addressing) {
        if let Some(peer) = addressing.peer {
            self.peers.entry(peer.as_uuid()).or_default().push(id);
        }
        if addressing.leading_mention {
            self.leading.push(id);
        }
    }
}

/// One owed row's stored fields: what its derivation reads.
type Owed<T> = (
    Uuid,
    serde_json::Value,
    <T as EventTable>::Content,
    serde_json::Value,
);

/// Up to [`BATCH`] owed rows of `T` within `scope`, newest first.
async fn owed_batch<T: EventTable>(
    db: &DatabaseConnection,
    scope: Condition,
) -> Result<Vec<Owed<T>>, StoreError> {
    let mut owed = T::find();
    for column in T::ORDER {
        owed = owed.order_by(*column, Order::Desc);
    }
    Ok(owed
        .select_only()
        .column(T::EVENT_ID)
        .column(T::TARGET)
        .column(T::CONTENT)
        .column(T::HEADERS)
        .filter(scope)
        .filter(T::DERIVED_VERSION.lt(DERIVED_VERSION))
        .limit(BATCH)
        .into_tuple()
        .all(db)
        .await?)
}

/// Derive `batch` in one transaction (the caller holds the write gate); how many rows it
/// derived. The batch was found without the gate, so only its rows still present and still
/// owed are written: a row another writer deleted or derived meanwhile is left alone.
async fn write_batch<T: EventTable>(
    db: &DatabaseConnection,
    batch: Vec<Owed<T>>,
) -> Result<usize, StoreError>
where
    <T::Headers as EntityTrait>::ActiveModel: Send,
{
    let txn = db.begin().await?;
    let still_owed: std::collections::HashSet<Uuid> =
        still_owed::<T>(&txn, batch.iter().map(|(id, ..)| *id).collect()).await?;
    let mut ids = Vec::with_capacity(still_owed.len());
    let mut grouped = Grouped::default();
    let mut headers = Vec::with_capacity(still_owed.len());
    for (id, target, content, held) in batch {
        if !still_owed.contains(&id) {
            continue;
        }
        let held: airc_core::Headers = serde_json::from_value(held)
            .map_err(|e| StoreError::Migration(format!("decoding headers: {e}")))?;
        grouped.add(id, T::addressing(target, content)?);
        headers.push((id, held));
        ids.push(id);
    }
    let count = ids.len();
    T::Headers::delete_many()
        .filter(T::HEADER_EVENT_ID.is_in(ids.clone()))
        .exec(&txn)
        .await?;
    write_header_rows::<T>(&txn, headers.iter().map(|(id, held)| (*id, held))).await?;
    T::update_many()
        .col_expr(T::ADDRESSED_PEER, Expr::value(Option::<Uuid>::None))
        .col_expr(T::LEADING_MENTION, Expr::value(false))
        .col_expr(T::DERIVED_VERSION, Expr::value(DERIVED_VERSION))
        .filter(T::EVENT_ID.is_in(ids))
        .exec(&txn)
        .await?;
    for (peer, ids) in grouped.peers {
        T::update_many()
            .col_expr(T::ADDRESSED_PEER, Expr::value(peer))
            .filter(T::EVENT_ID.is_in(ids))
            .exec(&txn)
            .await?;
    }
    if !grouped.leading.is_empty() {
        T::update_many()
            .col_expr(T::LEADING_MENTION, Expr::value(true))
            .filter(T::EVENT_ID.is_in(grouped.leading))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    Ok(count)
}

/// Which of `ids` are still rows of `T` that owe their derivation.
async fn still_owed<T: EventTable>(
    db: &impl ConnectionTrait,
    ids: Vec<Uuid>,
) -> Result<std::collections::HashSet<Uuid>, StoreError> {
    let rows: Vec<Uuid> = T::find()
        .select_only()
        .column(T::EVENT_ID)
        .filter(T::EVENT_ID.is_in(ids))
        .filter(T::DERIVED_VERSION.lt(DERIVED_VERSION))
        .into_tuple()
        .all(db)
        .await?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{bus_event_header, event_header};
    use crate::migration::{apply_migrations, Migrator};
    use crate::SqliteEventStore;
    use airc_core::{Body, EventId, MentionTarget, PeerId, TranscriptCursor};
    use sea_orm::ActiveValue::{NotSet, Set};
    use sea_orm::{Database, PaginatorTrait};
    use sea_orm_migration::seaql_migrations;
    use sea_orm_migration::{MigratorTrait, SchemaManager};
    use std::path::Path;

    /// The migrations before this change (000021 adds the derived columns).
    const BEFORE_DERIVED: u32 = 20;

    async fn connect(path: &Path) -> DatabaseConnection {
        Database::connect(crate::sqlite::sqlite_file_url(path))
            .await
            .unwrap()
    }

    async fn applied(db: &DatabaseConnection) -> u64 {
        seaql_migrations::Entity::find().count(db).await.unwrap()
    }

    async fn not_current(db: &DatabaseConnection) -> u64 {
        event::Entity::find()
            .filter(event::Column::DerivedVersion.lt(DERIVED_VERSION))
            .count(db)
            .await
            .unwrap()
    }

    /// An `events` row as a writer that knows nothing of the derived data
    /// inserts it: those columns are left to the schema (absent before 000021,
    /// their defaults after it).
    fn old_row(n: u128, target: MentionTarget, text: &str) -> event::ActiveModel {
        event::ActiveModel {
            event_id: Set(Uuid::from_u128(n)),
            room_id: Set(Uuid::from_u128(7)),
            peer_id: Set(Uuid::from_u128(8)),
            client_id: Set(Uuid::from_u128(9)),
            kind: Set("message".into()),
            occurred_at_ms: Set(n as i64),
            lamport: Set(n as i64),
            target: Set(serde_json::to_value(target).unwrap()),
            headers: Set(serde_json::json!({ "k": format!("v{n}") })),
            body: Set(Some(serde_json::to_value(Body::text(text)).unwrap())),
            attachment: Set(None),
            receipt: Set(None),
            metadata: Set(serde_json::Value::Null),
            addressed_peer: NotSet,
            leading_mention: NotSet,
            derived_version: NotSet,
        }
    }

    fn origin() -> TranscriptCursor {
        TranscriptCursor {
            lamport: 0,
            event_id: EventId::from_u128(0),
        }
    }

    fn newest() -> TranscriptCursor {
        TranscriptCursor {
            lamport: u64::MAX >> 1,
            event_id: EventId::from_u128(u128::MAX),
        }
    }

    // what this catches (Codex on #1618): sea-orm-migration runs SQLite migrations
    // without a transaction, so a crash after 000021's DDL and before its version row
    // left a schema the retry could not re-apply (duplicate column). Interrupted there,
    // nothing of the step is left; the reopen applies it, derives no row (card 70055c76:
    // open does no data work), and the derivation completes the old row.
    #[tokio::test]
    async fn a_fault_after_the_ddl_rolls_back_and_the_reopen_completes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let me = PeerId::from_u128(0x3e);
        let db = connect(&path).await;
        Migrator::up(&db, Some(BEFORE_DERIVED)).await.unwrap();
        event::Entity::insert(old_row(1, MentionTarget::Peer(me), "@Fable hi"))
            .exec(&db)
            .await
            .unwrap();
        fault::arm(Fault::AfterAddressingDdl);
        assert!(apply_migrations(&db).await.is_err());
        assert!(
            !SchemaManager::new(&db)
                .has_column("events", "addressed_peer")
                .await
                .unwrap(),
            "the DDL rolled back with its version row"
        );
        assert_eq!(applied(&db).await, u64::from(BEFORE_DERIVED));
        drop(db);
        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let db = store.connection();
        assert_eq!(applied(db).await, Migrator::migrations().len() as u64);
        assert_eq!(not_current(db).await, 1, "the open derived nothing");
        assert_eq!(store.derive_all_owed().await.unwrap(), 1);
        let row = event::Entity::find_by_id(Uuid::from_u128(1))
            .one(db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.addressed_peer, row.leading_mention, row.derived_version),
            (Some(me.as_uuid()), true, DERIVED_VERSION)
        );
        assert_eq!(event_header::Entity::find().count(db).await.unwrap(), 1);
    }

    // what this catches (Codex on #1618): a derivation interrupted midway is never
    // silently skipped. One batch commits, the pass fails; the next pass resumes, and
    // every row ends current with the right addressing and header rows. The open in
    // between derives nothing.
    #[tokio::test]
    async fn a_fault_midway_through_derivation_resumes_on_the_next_pass() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let me = PeerId::from_u128(0x3e);
        let db = connect(&path).await;
        Migrator::up(&db, Some(BEFORE_DERIVED)).await.unwrap();
        let rows: Vec<_> = (1..=2500u128)
            .map(|n| {
                if n % 2 == 0 {
                    old_row(n, MentionTarget::Peer(me), "plain")
                } else {
                    old_row(n, MentionTarget::All, "@Codex take it")
                }
            })
            .collect();
        for chunk in rows.chunks(500) {
            event::Entity::insert_many(chunk.to_vec())
                .exec(&db)
                .await
                .unwrap();
        }
        drop(db);
        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let db = store.connection();
        assert_eq!(not_current(db).await, 2500, "the open derived nothing");
        fault::arm(Fault::AfterDerivedBatch);
        assert!(store.derive_all_owed().await.is_err());
        assert_eq!(
            not_current(db).await,
            2500 - BATCH,
            "one batch derived, the rest still owed"
        );
        assert_eq!(store.derive_all_owed().await.unwrap() as u64, 2500 - BATCH);
        assert_eq!(not_current(db).await, 0, "every row derived");
        let addressed = event::Entity::find()
            .filter(event::Column::AddressedPeer.eq(me.as_uuid()))
            .count(db)
            .await
            .unwrap();
        let leading = event::Entity::find()
            .filter(event::Column::LeadingMention.eq(true))
            .count(db)
            .await
            .unwrap();
        assert_eq!((addressed, leading), (1250, 1250));
        assert_eq!(event_header::Entity::find().count(db).await.unwrap(), 2500);
    }

    // what this catches (Fable on #1654): a read that derived its own range took 99.8 s on a
    // 1.55 GB store before the first pass; and a filtered read that skipped owed rows silently
    // would let a resident miss an old DM with nobody able to tell why. A read never derives:
    // before the file settles, a filtered page and count over pre-migration rows say Unsettled;
    // after `settle`, Settled with the old match included. In both event tables; a plain page
    // is exact either way.
    #[tokio::test]
    async fn a_filtered_read_is_unsettled_until_the_derivation_at_open_settles_it() {
        use crate::{CountHistory, EventStore, SqliteDurableSink};
        use airc_bus::envelope::Cursor;
        use airc_bus::{DurableSink, Seq};
        use airc_core::{HeaderFilter, HistoryMatch, RoomId, Settlement};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let me = PeerId::from_u128(0x3e);
        let room = RoomId::from_u128(7);
        let db = connect(&path).await;
        Migrator::up(&db, Some(BEFORE_DERIVED)).await.unwrap();
        event::Entity::insert(old_row(1, MentionTarget::Peer(me), "plain"))
            .exec(&db)
            .await
            .unwrap();
        drop(db);
        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let db = store.connection();
        bus_event::Entity::insert(bus_event::ActiveModel {
            event_id: Set(Uuid::from_u128(2)),
            room_id: Set(room.as_uuid()),
            epoch: Set(1),
            counter: Set(1),
            kind: Set("message".into()),
            delivery: Set("durable".into()),
            target: Set(serde_json::json!("all")),
            correlation_id: Set(None),
            coalesce_key: Set(None),
            headers: Set(serde_json::json!({ "k": "v" })),
            payload: Set(Body::text("@Fable look").to_payload()),
            peer_id: Set(Uuid::from_u128(8)),
            client_id: Set(Uuid::from_u128(9)),
            occurred_at_ms: Set(1),
            addressed_peer: NotSet,
            leading_mention: NotSet,
            derived_version: NotSet,
        })
        .exec(db)
        .await
        .unwrap();
        let sink = SqliteDurableSink::open_path(&path).await.unwrap();
        let keyed = HeaderFilter::Has { key: "k".into() };
        let bus_origin = Cursor::new(Seq::new(0, 0), EventId::from_u128(0));
        let bus_through = Cursor::new(Seq::new(1, 1), EventId::from_u128(2));

        let plain = store
            .matching_history(
                &origin(),
                &newest(),
                &[room],
                HistoryMatch::headers(&HeaderFilter::Any),
                10,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            (plain.rows.len(), plain.settlement),
            (1, Settlement::Settled)
        );
        let page = store
            .matching_history(
                &origin(),
                &newest(),
                &[room],
                HistoryMatch::headers(&keyed),
                10,
                true,
            )
            .await
            .unwrap();
        // Unsettled, the read walks its newest span and matches it from each event, owed or not:
        // this store is small enough that the span holds the old row (crate::settling).
        assert_eq!(
            (page.rows.len(), page.settlement),
            (1, Settlement::Unsettled)
        );
        let count = store
            .count_history(&origin(), &newest(), &[room], &keyed, None)
            .await
            .unwrap();
        assert_eq!((count.count, count.settlement), (1, Settlement::Unsettled));
        let bus = sink
            .page_matching(
                room,
                bus_origin,
                bus_through,
                HistoryMatch::headers(&keyed),
                10,
                true,
            )
            .await
            .unwrap();
        assert_eq!((bus.rows.len(), bus.settlement), (1, Settlement::Unsettled));
        assert_eq!(not_current(db).await, 1, "a read derives nothing");

        assert_eq!(store.settle().await.unwrap(), 2, "one row of each table");
        assert_eq!(bus_event_header::Entity::find().count(db).await.unwrap(), 1);
        let page = store
            .matching_history(
                &origin(),
                &newest(),
                &[room],
                HistoryMatch::headers(&keyed),
                10,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            page.settlement,
            Settlement::Unsettled,
            "derived, not yet indexed"
        );
        store.ensure_indexes().await.unwrap();
        let page = store
            .matching_history(
                &origin(),
                &newest(),
                &[room],
                HistoryMatch::headers(&keyed),
                10,
                true,
            )
            .await
            .unwrap();
        assert_eq!((page.rows.len(), page.settlement), (1, Settlement::Settled));
        let row = event::Entity::find_by_id(Uuid::from_u128(1))
            .one(db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.addressed_peer, row.derived_version),
            (Some(me.as_uuid()), DERIVED_VERSION)
        );
        let count = store
            .count_history(&origin(), &newest(), &[room], &keyed, None)
            .await
            .unwrap();
        assert_eq!((count.count, count.settlement), (1, Settlement::Settled));
        let bus = sink
            .page_matching(
                room,
                bus_origin,
                bus_through,
                HistoryMatch::headers(&keyed),
                10,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            (bus.rows.len(), bus.settlement),
            (1, Settlement::Settled),
            "the sink shares the file's settlement"
        );
        let bus_count = sink
            .count_matching(room, bus_origin, bus_through, &keyed, None)
            .await
            .unwrap();
        assert_eq!(
            (bus_count.count, bus_count.settlement),
            (1, Settlement::Settled)
        );
    }

    // what this catches (Fable on #1654): settling a resident store derived the 2.2M rows its
    // next pass deletes. `settle` derives only the rows retention keeps (the drain's own
    // predicate, `Dropped`); the rest stay owed for the drain, and the filtered page is exact.
    #[tokio::test]
    async fn settling_derives_only_what_retention_keeps() {
        use crate::EventStore;
        use airc_core::{HeaderFilter, HistoryMatch, RoomId, Settlement};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let (dropped, kept) = (4000u128, 1000u128);
        let db = connect(&path).await;
        Migrator::up(&db, Some(BEFORE_DERIVED)).await.unwrap();
        let rows: Vec<_> = (1..=dropped + kept)
            .map(|n| {
                let mut row = old_row(n, MentionTarget::All, "plain");
                if n <= dropped {
                    // One peer's identity republished: only the newest survives the drain.
                    row.kind = Set("identity_published".into());
                    row.headers = Set(serde_json::json!({ "k": "stale" }));
                } else if n % 300 == 0 {
                    row.headers = Set(serde_json::json!({ "k": "v" }));
                } else {
                    row.headers = Set(serde_json::json!({ "other": "v" }));
                }
                row
            })
            .collect();
        for chunk in rows.chunks(500) {
            event::Entity::insert_many(chunk.to_vec())
                .exec(&db)
                .await
                .unwrap();
        }
        drop(db);
        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let db = store.connection();
        assert_eq!(store.settle().await.unwrap() as u128, kept + 1);
        store.ensure_indexes().await.unwrap();
        assert_eq!(
            not_current(db).await,
            (dropped - 1) as u64,
            "every row the drain drops still owed; the kept rows (and the newest identity) derived"
        );
        let keyed = HeaderFilter::Exact {
            key: "k".into(),
            value: "v".into(),
        };
        let page = store
            .matching_history(
                &origin(),
                &newest(),
                &[RoomId::from_u128(7)],
                HistoryMatch::headers(&keyed),
                50,
                true,
            )
            .await
            .unwrap();
        let lamports: Vec<u64> = page.rows.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, [4200, 4500, 4800], "short, and exact");
        assert_eq!(page.settlement, Settlement::Settled);
    }

    // what this catches (Fable on #1654): a filtered read on a store with no indexes yet walked
    // all its rows (62.6 s on a copy of a resident's 2.2M-row store, 13.2 s even once derived).
    // Unsettled, it walks only its newest rows, so a match older than the walk's reach is
    // missing and says so; once the file is derived AND indexed it is Settled and exact, and the
    // settled signal fires then, once, not before.
    #[tokio::test]
    async fn an_unsettled_read_walks_only_its_newest_rows_and_settles_when_derived_and_indexed() {
        use crate::EventStore;
        use airc_core::{HeaderFilter, HistoryMatch, RoomId, Settlement};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let rows = 3000u128;
        let db = connect(&path).await;
        Migrator::up(&db, Some(BEFORE_DERIVED)).await.unwrap();
        let batch: Vec<_> = (1..=rows)
            .map(|n| {
                let mut row = old_row(n, MentionTarget::All, "plain");
                let wanted = n == 1 || n == rows;
                row.headers = Set(if wanted {
                    serde_json::json!({ "k": "v" })
                } else {
                    serde_json::json!({ "other": "v" })
                });
                row
            })
            .collect();
        for chunk in batch.chunks(500) {
            event::Entity::insert_many(chunk.to_vec())
                .exec(&db)
                .await
                .unwrap();
        }
        drop(db);
        // Derived already (as on a store a previous binary derived), but never indexed.
        let first = SqliteEventStore::open_path(&path).await.unwrap();
        first.derive_all_owed().await.unwrap();
        crate::indexes::drop_all(&SchemaManager::new(first.connection()))
            .await
            .unwrap();
        drop(first);

        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let mut settled = store.settled();
        let keyed = HeaderFilter::Has { key: "k".into() };
        let (from, to, rooms) = (origin(), newest(), [RoomId::from_u128(7)]);
        let read =
            || store.matching_history(&from, &to, &rooms, HistoryMatch::headers(&keyed), 10, true);
        let page = read().await.unwrap();
        let lamports: Vec<u64> = page.rows.iter().map(|e| e.lamport).collect();
        assert_eq!(
            (lamports, page.settlement),
            (vec![rows as u64], Settlement::Unsettled),
            "the newest match, not the one beyond the walk's reach"
        );

        store.settle().await.unwrap();
        assert!(
            !settled.has_changed().unwrap(),
            "derived alone does not settle"
        );
        store.ensure_indexes().await.unwrap();
        assert!(
            settled.has_changed().unwrap(),
            "derived and indexed settles"
        );
        assert_eq!(*settled.borrow_and_update(), Settlement::Settled);
        let page = read().await.unwrap();
        let lamports: Vec<u64> = page.rows.iter().map(|e| e.lamport).collect();
        assert_eq!(
            (lamports, page.settlement),
            (vec![1, rows as u64], Settlement::Settled)
        );
        store.ensure_indexes().await.unwrap();
        assert!(!settled.has_changed().unwrap(), "it fires once");
    }
}
