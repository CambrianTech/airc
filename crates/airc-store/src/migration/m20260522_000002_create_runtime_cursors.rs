//! Add durable runtime cursor checkpoints.
//!
//! This replaces ad-hoc JSON cursor sidecars for hook/feed consumers.
//! The row is keyed by logical consumer id and stores the canonical
//! transcript cursor `(lamport, event_id)`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RuntimeCursors::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RuntimeCursors::ConsumerId)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(RuntimeCursors::Lamport)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(RuntimeCursors::EventId).uuid().not_null())
                    .col(
                        ColumnDef::new(RuntimeCursors::UpdatedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RuntimeCursors::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RuntimeCursors {
    Table,
    ConsumerId,
    Lamport,
    EventId,
    UpdatedAtMs,
}
