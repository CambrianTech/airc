//! Create the `bus_epoch` table — the persisted generational epoch cell
//! (§3.8 of `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! A single row (`id = 0`) holding the owner-core's `epoch`. `SeqSource`
//! bumps it once per daemon start so that, after a deliver-first crash
//! that lost an un-flushed counter tail, every post-restart event sorts
//! strictly after anything pre-restart even when the in-memory counter
//! rewinds. Backing it with the ORM (not a flat file) keeps the daemon's
//! durable state in one store. The in-memory `airc_bus::InMemoryEpochStore`
//! does NOT survive a restart, so the daemon must use the persisted cell.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(BusEpoch::Table)
                    .if_not_exists()
                    // Singleton: there is exactly one row, `id = 0`.
                    .col(
                        ColumnDef::new(BusEpoch::Id)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(BusEpoch::Epoch).big_integer().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BusEpoch::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BusEpoch {
    Table,
    Id,
    Epoch,
}
