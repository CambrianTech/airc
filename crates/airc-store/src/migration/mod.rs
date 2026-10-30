//! SeaORM migrations.
//!
//! Run automatically by `SqliteEventStore::open` so consumers don't
//! need a separate "init" step. Migrations preserve user data; most are
//! additive, and the few SQLite table-recreate migrations copy rows
//! before dropping their legacy tables. An older database is migrated
//! forward; a NEWER one (migrations this binary does not know) is refused for
//! writing ([`crate::StoreError::AheadOfBinary`]).

use sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait, PaginatorTrait, TransactionTrait};
use sea_orm_migration::prelude::*;
use sea_orm_migration::seaql_migrations;

use crate::error::StoreError;

pub(crate) mod fault;

mod m20260519_000001_create_events;
mod m20260522_000002_create_runtime_cursors;
mod m20260522_000003_create_peer_trust;
mod m20260522_000004_create_subscriptions;
mod m20260522_000005_create_local_identity;
mod m20260522_000006_create_mesh_identity;
mod m20260522_000007_create_account_registry;
mod m20260522_000008_create_beacons;
mod m20260522_000009_add_local_identity_card;
mod m20260523_000010_create_refresh_locks;
mod m20260526_000011_create_bus_events;
mod m20260527_000012_create_bus_epoch;
mod m20260528_000013_add_local_identity_agent_name;
mod m20260528_000014_drop_local_identity_singleton_check;
mod m20260529_000015_add_peer_trust_tier;
mod m20260610_000016_add_peer_trust_endpoints;
mod m20260614_000017_add_peer_trust_last_seen;
mod m20260629_000018_create_scoped_state;
mod m20260723_000019_add_peer_trust_endpoints_stamp;
mod m20260723_000020_add_peer_trust_endpoints_peer;
mod m20261029_000021_add_event_addressing;
mod m20261029_000022_create_event_headers;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260519_000001_create_events::Migration),
            Box::new(m20260522_000002_create_runtime_cursors::Migration),
            Box::new(m20260522_000003_create_peer_trust::Migration),
            Box::new(m20260522_000004_create_subscriptions::Migration),
            Box::new(m20260522_000005_create_local_identity::Migration),
            Box::new(m20260522_000006_create_mesh_identity::Migration),
            Box::new(m20260522_000007_create_account_registry::Migration),
            Box::new(m20260522_000008_create_beacons::Migration),
            Box::new(m20260522_000009_add_local_identity_card::Migration),
            Box::new(m20260523_000010_create_refresh_locks::Migration),
            Box::new(m20260526_000011_create_bus_events::Migration),
            Box::new(m20260527_000012_create_bus_epoch::Migration),
            Box::new(m20260528_000013_add_local_identity_agent_name::Migration),
            Box::new(m20260528_000014_drop_local_identity_singleton_check::Migration),
            Box::new(m20260529_000015_add_peer_trust_tier::Migration),
            Box::new(m20260610_000016_add_peer_trust_endpoints::Migration),
            Box::new(m20260614_000017_add_peer_trust_last_seen::Migration),
            Box::new(m20260629_000018_create_scoped_state::Migration),
            Box::new(m20260723_000019_add_peer_trust_endpoints_stamp::Migration),
            Box::new(m20260723_000020_add_peer_trust_endpoints_peer::Migration),
            Box::new(m20261029_000021_add_event_addressing::Migration),
            Box::new(m20261029_000022_create_event_headers::Migration),
        ]
    }
}

/// Bring the database to this binary's schema and derived data, for writing.
///
/// 1. Refuse a database migrated AHEAD of this binary: a newer build added
///    columns or tables it keeps current at every write (the addressing
///    columns, the header rows), and an older writer would insert rows without
///    them. The refusal names the unknown versions.
/// 2. Apply the pending migrations and their version rows in ONE transaction.
///    sea-orm-migration 1.1 runs SQLite migrations on the connection it is
///    given without a transaction of its own (`exec_with_connection`), so a
///    crash between a migration's DDL and its version row left a half-applied
///    schema that the retry could not re-apply (duplicate column). Given our
///    transaction, the DDL and the version rows commit together or not at all
///    (SQLite DDL is transactional).
/// 3. Derive what rows still owe ([`crate::derive::derive_pending`]): resumable,
///    so an interrupted derivation or rows an older writer inserted are healed
///    on this and every later open.
pub async fn apply_migrations(db: &DatabaseConnection) -> Result<(), StoreError> {
    let ahead = unknown_applied(db).await?;
    if !ahead.is_empty() {
        return Err(StoreError::AheadOfBinary { versions: ahead });
    }
    if !Migrator::get_pending_migrations(db).await?.is_empty() {
        let txn = db.begin().await?;
        Migrator::up(&txn, None).await?;
        txn.commit().await?;
    }
    crate::derive::derive_pending(db).await
}

/// Refuse a write when the database was migrated past this binary after it was
/// opened (an `airc update` ran while this process held the store open): the
/// write would store rows without what the newer binary derives. Cheap: one
/// COUNT of the version table, which at open held exactly this binary's
/// migrations; only a mismatch reads the versions to name them.
pub(crate) async fn refuse_if_ahead(db: &impl ConnectionTrait) -> Result<(), StoreError> {
    let applied = seaql_migrations::Entity::find().count(db).await?;
    if applied <= Migrator::migrations().len() as u64 {
        return Ok(());
    }
    Err(StoreError::AheadOfBinary {
        versions: unknown_applied(db).await?,
    })
}

/// Versions recorded as applied that this binary has no migration for.
async fn unknown_applied(db: &impl ConnectionTrait) -> Result<Vec<String>, StoreError> {
    Migrator::install(db).await?;
    let known: std::collections::HashSet<String> = Migrator::migrations()
        .iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    Ok(seaql_migrations::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.version)
        .filter(|version| !known.contains(version))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, Database, Set};

    /// Record `version` as applied, through SeaORM's own migrations entity.
    async fn record(db: &DatabaseConnection, version: &str) {
        seaql_migrations::ActiveModel {
            version: Set(version.to_owned()),
            applied_at: Set(0),
        }
        .insert(db)
        .await
        .expect("record migration");
    }

    async fn memory_db() -> DatabaseConnection {
        Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory")
    }

    // what this catches (Codex on #1618): a store this binary opened stays open
    // across an `airc update` that migrates the database forward; its next write
    // is refused AheadOfBinary, never a row without the newer derived data.
    #[tokio::test]
    async fn a_stale_writer_is_refused_after_the_database_moves_ahead() {
        use crate::store::EventStore;
        let store = crate::SqliteEventStore::in_memory().await.unwrap();
        let cursor = |n: u128| airc_core::TranscriptCursor {
            lamport: n as u64,
            event_id: airc_core::EventId::from_u128(n),
        };
        store.save_runtime_cursor("c", &cursor(1), 1).await.unwrap();
        record(store.connection(), "m99999999_999999_newer_binary").await;
        match store.save_runtime_cursor("c", &cursor(2), 2).await {
            Err(StoreError::AheadOfBinary { versions }) => {
                assert_eq!(versions, ["m99999999_999999_newer_binary"]);
            }
            other => panic!("expected AheadOfBinary, got {other:?}"),
        }
    }

    // what this catches (Codex on #1618): a database a NEWER binary migrated is refused
    // for writing, naming the versions, instead of tolerated: this binary would write rows
    // without the data the newer one derives at every write.
    #[tokio::test]
    async fn a_database_ahead_of_this_binary_is_refused_for_writing() {
        let db = memory_db().await;
        apply_migrations(&db).await.expect("initial migrate");
        record(&db, "m99999999_999999_future").await;
        match apply_migrations(&db).await {
            Err(StoreError::AheadOfBinary { versions }) => {
                assert_eq!(versions, ["m99999999_999999_future"]);
            }
            other => panic!("expected AheadOfBinary, got {other:?}"),
        }
    }
}
