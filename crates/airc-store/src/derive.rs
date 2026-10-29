//! Data the store derives from each stored event, kept current by one
//! resumable pass: the addressing columns ([`airc_core::Addressing`]) and the
//! header rows (`crate::header_query`) of `events` and `bus_events`.
//!
//! A row is current when its `derived_version` is [`DERIVED_VERSION`]. Appends
//! by this binary write it current. Rows at an older version (everything stored
//! before migration 000021, a derivation interrupted midway, or a row an older
//! writer inserted without the column) are derived here, a batch at a time,
//! each batch in one transaction: its header rows are replaced (deleted, then
//! inserted), its addressing recomputed and its version set, so a half-derived
//! row is never visible. [`derive_pending`] runs on every store open, so
//! nothing is silently skipped.

use std::collections::BTreeMap;

use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use uuid::Uuid;

use crate::entities::{bus_event, bus_event_header, event, event_header};
use crate::error::StoreError;
use crate::header_query::{bus_event_header_rows, event_header_rows, HEADER_ROWS_PER_INSERT};
use crate::migration::fault::{self, Fault};

/// The version of the derived data this binary writes.
pub(crate) const DERIVED_VERSION: i32 = 1;

/// Rows derived per transaction.
const BATCH: u64 = 1000;

/// Derive every row of both event tables that is not current.
pub(crate) async fn derive_pending(db: &DatabaseConnection) -> Result<(), StoreError> {
    loop {
        let events = derive_events(db).await?;
        let bus_events = derive_bus_events(db).await?;
        if events == 0 && bus_events == 0 {
            return Ok(());
        }
        fault::hit(Fault::AfterDerivedBatch)?;
    }
}

fn decode(what: &str, error: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("deriving {what}: {error}"))
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

async fn derive_events(db: &DatabaseConnection) -> Result<usize, StoreError> {
    let rows: Vec<(
        Uuid,
        serde_json::Value,
        Option<serde_json::Value>,
        serde_json::Value,
    )> = event::Entity::find()
        .select_only()
        .column(event::Column::EventId)
        .column(event::Column::Target)
        .column(event::Column::Body)
        .column(event::Column::Headers)
        .filter(event::Column::DerivedVersion.lt(DERIVED_VERSION))
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
    let mut headers = Vec::new();
    for (id, target, body, held) in rows {
        let target: airc_core::MentionTarget =
            serde_json::from_value(target).map_err(|e| decode("events.target", e))?;
        let body: Option<airc_core::Body> = body
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| decode("events.body", e))?;
        let held: airc_core::Headers =
            serde_json::from_value(held).map_err(|e| decode("events.headers", e))?;
        grouped.add(id, airc_core::Addressing::of_target(&target, body.as_ref()));
        headers.extend(event_header_rows(id, &held));
        ids.push(id);
    }
    let txn = db.begin().await?;
    event_header::Entity::delete_many()
        .filter(event_header::Column::EventId.is_in(ids.clone()))
        .exec(&txn)
        .await?;
    for chunk in headers.chunks(HEADER_ROWS_PER_INSERT) {
        event_header::Entity::insert_many(chunk.to_vec())
            .exec(&txn)
            .await?;
    }
    event::Entity::update_many()
        .col_expr(
            event::Column::AddressedPeer,
            Expr::value(Option::<Uuid>::None),
        )
        .col_expr(event::Column::LeadingMention, Expr::value(false))
        .col_expr(event::Column::DerivedVersion, Expr::value(DERIVED_VERSION))
        .filter(event::Column::EventId.is_in(ids))
        .exec(&txn)
        .await?;
    for (peer, ids) in grouped.peers {
        event::Entity::update_many()
            .col_expr(event::Column::AddressedPeer, Expr::value(peer))
            .filter(event::Column::EventId.is_in(ids))
            .exec(&txn)
            .await?;
    }
    if !grouped.leading.is_empty() {
        event::Entity::update_many()
            .col_expr(event::Column::LeadingMention, Expr::value(true))
            .filter(event::Column::EventId.is_in(grouped.leading))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    Ok(count)
}

async fn derive_bus_events(db: &DatabaseConnection) -> Result<usize, StoreError> {
    let rows: Vec<(Uuid, serde_json::Value, Vec<u8>, serde_json::Value)> =
        bus_event::Entity::find()
            .select_only()
            .column(bus_event::Column::EventId)
            .column(bus_event::Column::Target)
            .column(bus_event::Column::Payload)
            .column(bus_event::Column::Headers)
            .filter(bus_event::Column::DerivedVersion.lt(DERIVED_VERSION))
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
    let mut headers = Vec::new();
    for (id, target, payload, held) in rows {
        let target: airc_bus::Target =
            serde_json::from_value(target).map_err(|e| decode("bus_events.target", e))?;
        let held: airc_core::Headers =
            serde_json::from_value(held).map_err(|e| decode("bus_events.headers", e))?;
        grouped.add(id, airc_bus::addressing_of(&target, &payload));
        headers.extend(bus_event_header_rows(id, &held));
        ids.push(id);
    }
    let txn = db.begin().await?;
    bus_event_header::Entity::delete_many()
        .filter(bus_event_header::Column::EventId.is_in(ids.clone()))
        .exec(&txn)
        .await?;
    for chunk in headers.chunks(HEADER_ROWS_PER_INSERT) {
        bus_event_header::Entity::insert_many(chunk.to_vec())
            .exec(&txn)
            .await?;
    }
    bus_event::Entity::update_many()
        .col_expr(
            bus_event::Column::AddressedPeer,
            Expr::value(Option::<Uuid>::None),
        )
        .col_expr(bus_event::Column::LeadingMention, Expr::value(false))
        .col_expr(
            bus_event::Column::DerivedVersion,
            Expr::value(DERIVED_VERSION),
        )
        .filter(bus_event::Column::EventId.is_in(ids))
        .exec(&txn)
        .await?;
    for (peer, ids) in grouped.peers {
        bus_event::Entity::update_many()
            .col_expr(bus_event::Column::AddressedPeer, Expr::value(peer))
            .filter(bus_event::Column::EventId.is_in(ids))
            .exec(&txn)
            .await?;
    }
    if !grouped.leading.is_empty() {
        bus_event::Entity::update_many()
            .col_expr(bus_event::Column::LeadingMention, Expr::value(true))
            .filter(bus_event::Column::EventId.is_in(grouped.leading))
            .exec(&txn)
            .await?;
    }
    txn.commit().await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::{apply_migrations, Migrator};
    use crate::SqliteEventStore;
    use airc_core::{Body, MentionTarget, PeerId};
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

    // what this catches (Codex on #1618): sea-orm-migration runs SQLite migrations
    // without a transaction, so a crash after 000021's DDL and before its version row
    // left a schema the retry could not re-apply (duplicate column). Interrupted there,
    // nothing of the step is left; the reopen applies it and derives the old row.
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
        let db = connect(&path).await;
        apply_migrations(&db).await.unwrap();
        assert_eq!(applied(&db).await, Migrator::migrations().len() as u64);
        let row = event::Entity::find_by_id(Uuid::from_u128(1))
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.addressed_peer, row.leading_mention, row.derived_version),
            (Some(me.as_uuid()), true, DERIVED_VERSION)
        );
        assert_eq!(event_header::Entity::find().count(&db).await.unwrap(), 1);
    }

    // what this catches (Codex on #1618): a derivation interrupted midway is never
    // silently skipped. One batch commits, the open fails; the reopen resumes, and
    // every row ends current with the right addressing and header rows.
    #[tokio::test]
    async fn a_fault_midway_through_derivation_resumes_on_reopen() {
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
        fault::arm(Fault::AfterDerivedBatch);
        assert!(apply_migrations(&db).await.is_err());
        assert_eq!(
            not_current(&db).await,
            2500 - BATCH,
            "one batch derived, the rest still owed"
        );
        drop(db);
        let db = connect(&path).await;
        apply_migrations(&db).await.unwrap();
        assert_eq!(not_current(&db).await, 0, "every row derived");
        let addressed = event::Entity::find()
            .filter(event::Column::AddressedPeer.eq(me.as_uuid()))
            .count(&db)
            .await
            .unwrap();
        let leading = event::Entity::find()
            .filter(event::Column::LeadingMention.eq(true))
            .count(&db)
            .await
            .unwrap();
        assert_eq!((addressed, leading), (1250, 1250));
        assert_eq!(event_header::Entity::find().count(&db).await.unwrap(), 2500);
    }

    // what this catches (Codex on #1618): a row an older writer inserts without the
    // derived data (`derived_version` 0, no header rows) is derived on the next open,
    // in both event tables.
    #[tokio::test]
    async fn rows_an_older_writer_inserted_are_derived_on_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let me = PeerId::from_u128(0x3e);
        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let db = store.connection();
        event::Entity::insert(old_row(1, MentionTarget::Peer(me), "plain"))
            .exec(db)
            .await
            .unwrap();
        bus_event::Entity::insert(bus_event::ActiveModel {
            event_id: Set(Uuid::from_u128(2)),
            room_id: Set(Uuid::from_u128(7)),
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
        assert_eq!(not_current(db).await, 1, "the old writer's row is owed");
        drop(store);
        let store = SqliteEventStore::open_path(&path).await.unwrap();
        let db = store.connection();
        let row = event::Entity::find_by_id(Uuid::from_u128(1))
            .one(db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.addressed_peer, row.derived_version),
            (Some(me.as_uuid()), DERIVED_VERSION)
        );
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
