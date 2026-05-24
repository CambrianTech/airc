//! SeaORM migrations.
//!
//! Run automatically by `SqliteEventStore::open` so consumers don't
//! need a separate "init" step. Each migration is an additive change
//! (new table / new column / new index) and never destructive — the
//! store treats older databases as forward-compatible.

use sea_orm_migration::prelude::*;

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
        ]
    }
}
