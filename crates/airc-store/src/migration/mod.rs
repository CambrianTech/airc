//! SeaORM migrations.
//!
//! Run automatically by `SqliteEventStore::open` so consumers don't
//! need a separate "init" step. Each migration is an additive change
//! (new table / new column / new index) and never destructive — the
//! store treats older databases as forward-compatible.

use sea_orm_migration::prelude::*;

mod m20260519_000001_create_events;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260519_000001_create_events::Migration)]
    }
}
