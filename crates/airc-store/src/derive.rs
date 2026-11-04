//! Data the store derives from each stored event: the addressing columns
//! ([`airc_core::Addressing`]) and the header rows (`crate::header_query`) of
//! `events` and `bus_events`.
//!
//! A row is current when its `derived_version` is [`DERIVED_VERSION`]. Appends
//! by this binary write it current. A row at an older version (stored before
//! migration 000021, or inserted by an older writer without the column) is
//! derived a batch at a time, each batch in one transaction under the file's
//! write gate: its header rows are replaced (deleted, then inserted), its
//! addressing recomputed and its version set, so a half-derived row is never
//! visible.
//!
//! Nothing derives at open (card 70055c76: open did the whole table, 23.4 s on a
//! 1.56 GB store). Who derives, and only what they reach:
//! - a read whose predicate reads derived data ([`reads_derived`]) derives the
//!   owed rows of its own range first ([`derive_owed`]), so its answer is exact;
//!   a read that does not (the plain newest page) derives nothing;
//! - a count derives nothing: owed rows in its range make it a lower bound
//!   (`HistoryCount::more`, [`owes`]);
//! - the store's background pass derives every owed row after its drain, when
//!   the table is small (`crate::retention::run::pass`).

use std::collections::BTreeMap;
use std::future::Future;

use airc_core::HeaderFilter;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect,
    TransactionTrait,
};
use uuid::Uuid;

use crate::entities::{bus_event, event};
use crate::error::StoreError;
use crate::event_table::EventTable;
use crate::header_query::write_header_rows;
use crate::migration::fault::{self, Fault};
use crate::sqlite::SqliteEventStore;

/// The version of the derived data this binary writes.
pub(crate) const DERIVED_VERSION: i32 = 1;

/// Rows derived per transaction.
const BATCH: u64 = 1000;

/// Whether a read's predicate reads derived data: a header predicate and the exclusion of a
/// runtime's own sends read the header rows, an addressee filter the addressing columns.
pub(crate) fn reads_derived(headers: &HeaderFilter, addressee: bool, exclude: bool) -> bool {
    !matches!(headers, HeaderFilter::Any) || addressee || exclude
}

/// Whether any row of `T` within `scope` still owes its derivation.
pub(crate) async fn owes<T: EventTable>(
    db: &DatabaseConnection,
    scope: Condition,
) -> Result<bool, StoreError> {
    let owed: Option<Uuid> = T::find()
        .select_only()
        .column(T::EVENT_ID)
        .filter(scope)
        .filter(T::DERIVED_VERSION.lt(DERIVED_VERSION))
        .limit(1)
        .into_tuple()
        .one(db)
        .await?;
    Ok(owed.is_some())
}

/// Derive every owed row of `T` within `scope`, one batch per transaction, each under the
/// write admission `admit` returns (the file's gate, held for that batch only); how many.
/// `lift` is the caller's error for a store failure (the sink's is `BusError::sink_source`).
pub(crate) async fn derive_owed<T, G, E, Admit, Admitted>(
    db: &DatabaseConnection,
    scope: Condition,
    admit: Admit,
    lift: fn(StoreError) -> E,
) -> Result<usize, E>
where
    T: EventTable,
    <T::Headers as EntityTrait>::ActiveModel: Send,
    Admit: Fn() -> Admitted,
    Admitted: Future<Output = Result<G, E>>,
{
    let mut derived = 0;
    loop {
        let write = admit().await?;
        let count = derive_batch::<T>(db, scope.clone()).await.map_err(lift)?;
        drop(write);
        if count == 0 {
            return Ok(derived);
        }
        derived += count;
        fault::hit(Fault::AfterDerivedBatch).map_err(|e| lift(e.into()))?;
    }
}

impl SqliteEventStore {
    /// Derive every owed row of both event tables ([`derive_owed`]): the background pass's step
    /// after its drain, when the tables are small. How many rows it derived.
    pub async fn derive_all_owed(&self) -> Result<usize, StoreError> {
        let events = derive_owed::<event::Entity, _, _, _, _>(
            self.connection(),
            Condition::all(),
            || self.admit_write(),
            std::convert::identity,
        )
        .await?;
        let bus_events = derive_owed::<bus_event::Entity, _, _, _, _>(
            self.connection(),
            Condition::all(),
            || self.admit_write(),
            std::convert::identity,
        )
        .await?;
        Ok(events + bus_events)
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

/// Derive up to [`BATCH`] owed rows of `T` within `scope`, in one transaction;
/// how many there were.
async fn derive_batch<T: EventTable>(
    db: &DatabaseConnection,
    scope: Condition,
) -> Result<usize, StoreError>
where
    <T::Headers as EntityTrait>::ActiveModel: Send,
{
    let rows: Vec<(Uuid, serde_json::Value, T::Content, serde_json::Value)> = T::find()
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
        .await?;
    let count = rows.len();
    if count == 0 {
        return Ok(0);
    }
    let mut ids = Vec::with_capacity(count);
    let mut grouped = Grouped::default();
    let mut headers = Vec::with_capacity(count);
    for (id, target, content, held) in rows {
        let held: airc_core::Headers = serde_json::from_value(held)
            .map_err(|e| StoreError::Migration(format!("decoding headers: {e}")))?;
        grouped.add(id, T::addressing(target, content)?);
        headers.push((id, held));
        ids.push(id);
    }
    let txn = db.begin().await?;
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

    // what this catches (card 70055c76): a row an older writer inserts without the derived
    // data (`derived_version` 0, no header rows) is answered exactly by every read that
    // depends on it, in both event tables: a filtered read derives its own range first; a
    // count derives nothing and says it is a lower bound; a plain page derives nothing.
    #[tokio::test]
    async fn owed_rows_are_derived_by_the_read_that_reaches_them() {
        use crate::{CountHistory, EventStore, SqliteDurableSink};
        use airc_bus::envelope::Cursor;
        use airc_bus::{DurableSink, Seq};
        use airc_core::{HeaderFilter, HistoryMatch, RoomId};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let me = PeerId::from_u128(0x3e);
        let room = RoomId::from_u128(7);
        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let db = store.connection();
        event::Entity::insert(old_row(1, MentionTarget::Peer(me), "plain"))
            .exec(db)
            .await
            .unwrap();
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
        let keyed = HeaderFilter::Has { key: "k".into() };

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
        assert_eq!(plain.len(), 1);
        assert_eq!(not_current(db).await, 1, "a plain page derives nothing");

        let count = store
            .count_history(&origin(), &newest(), &[room], &keyed, None)
            .await
            .unwrap();
        assert!(count.more, "owed rows in range: the count is a lower bound");
        assert_eq!(not_current(db).await, 1, "a count derives nothing");

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
            page.len(),
            1,
            "the filtered read derived its range, then matched"
        );
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
        assert_eq!((count.count, count.more), (1, false), "exact once derived");

        let sink = SqliteDurableSink::open_path(&path).await.unwrap();
        let bus_origin = Cursor::new(Seq::new(0, 0), EventId::from_u128(0));
        let bus_through = Cursor::new(Seq::new(1, 1), EventId::from_u128(2));
        let count = sink
            .count_matching(room, bus_origin, bus_through, &keyed, None)
            .await
            .unwrap();
        assert!(count.more, "the sink's count is a lower bound too");
        let page = sink
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
        assert_eq!(page.len(), 1);
        let bus = bus_event::Entity::find_by_id(Uuid::from_u128(2))
            .one(db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (bus.leading_mention, bus.derived_version),
            (true, DERIVED_VERSION)
        );
        assert_eq!(event_header::Entity::find().count(db).await.unwrap(), 1);
        assert_eq!(bus_event_header::Entity::find().count(db).await.unwrap(), 1);
    }
}
