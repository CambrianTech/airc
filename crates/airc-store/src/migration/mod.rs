//! SeaORM migrations.
//!
//! Run automatically by `SqliteEventStore::open` so consumers don't
//! need a separate "init" step. Migrations preserve user data; most are
//! additive, and the few SQLite table-recreate migrations copy rows
//! before dropping their legacy tables. The store treats older
//! databases as forward-compatible.

use sea_orm::{DatabaseConnection, EntityTrait};
use sea_orm_migration::prelude::*;
use sea_orm_migration::seaql_migrations;

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
        ]
    }
}

/// Apply pending migrations, tolerating a database that is AHEAD of this
/// binary.
///
/// `Migrator::up` hard-fails when the DB has applied migrations this
/// binary has no file for (SeaORM: "migration file is missing"). That
/// happens under version skew — a newer build migrates the DB forward,
/// then an older binary opens it (e.g. a stale `~/.local/bin/airc`
/// against a DB a dev build already migrated). Our migrations preserve
/// existing data and older binaries simply ignore schema they do not
/// understand, so a DB ahead of us is harmless: the extra tables/columns
/// are unused. If `up` fails ONLY because the DB is ahead — every
/// migration WE define is already applied — we proceed. A genuine
/// divergence (we still have unapplied migrations of our own) surfaces
/// the error instead of silently skipping schema we actually need.
pub async fn apply_migrations(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    match Migrator::up(db, None).await {
        Ok(()) => Ok(()),
        Err(err) => {
            if all_defined_migrations_applied(db).await? {
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

/// True when every migration this binary defines is already recorded in
/// the DB's `seaql_migrations` table — i.e. the DB is at or ahead of us,
/// never behind on one of ours.
async fn all_defined_migrations_applied(db: &DatabaseConnection) -> Result<bool, sea_orm::DbErr> {
    // Read the applied set through SeaORM's own `seaql_migrations`
    // entity — not raw SQL (the migration table is a first-class entity;
    // hand-written SQL here would be a "bad ORM" smell + violate the
    // no-raw-SQL rule).
    let applied: std::collections::HashSet<String> = seaql_migrations::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.version)
        .collect();
    Ok(Migrator::migrations()
        .iter()
        .all(|migration| applied.contains(migration.name())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

    async fn memory_db() -> DatabaseConnection {
        Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory")
    }

    #[tokio::test]
    async fn tolerates_a_db_migrated_ahead_of_this_binary() {
        let db = memory_db().await;
        apply_migrations(&db).await.expect("initial migrate");
        // Simulate a newer build recording a migration we have no file for.
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO seaql_migrations (version, applied_at) \
             VALUES ('m99999999_999999_future', 0)"
                .to_owned(),
        ))
        .await
        .expect("insert future migration");
        // Re-opening must NOT hard-fail on the unknown-applied migration —
        // the DB is merely ahead of us, and our schema is all present.
        apply_migrations(&db)
            .await
            .expect("must tolerate a DB ahead of this binary");
    }

    #[tokio::test]
    async fn surfaces_error_when_our_own_migrations_are_unapplied() {
        // A DB whose ONLY recorded migration is foreign (none of ours) —
        // we genuinely have pending schema, so we must NOT silently skip
        // it; the error surfaces instead.
        let db = memory_db().await;
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "CREATE TABLE seaql_migrations \
             (version varchar PRIMARY KEY, applied_at bigint NOT NULL)"
                .to_owned(),
        ))
        .await
        .expect("create migrations table");
        db.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO seaql_migrations (version, applied_at) \
             VALUES ('m99999999_999999_foreign', 0)"
                .to_owned(),
        ))
        .await
        .expect("insert foreign migration");
        assert!(
            apply_migrations(&db).await.is_err(),
            "divergence (our migrations unapplied) must surface, not silently skip"
        );
    }
}
