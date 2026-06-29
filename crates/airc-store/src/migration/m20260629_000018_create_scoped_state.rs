//! Add the generic `scoped_state` table — a key→JSON store scoped to a
//! user, a room, or a `(user, room)` pair. See `airc_store::scoped_state`.
//!
//! Composite primary key `(scope_key, key)`. Its index's leftmost
//! prefix (`scope_key`) covers the `list_scoped_state` range scan, so
//! no separate secondary index is created.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ScopedState::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ScopedState::ScopeKey).string().not_null())
                    .col(ColumnDef::new(ScopedState::Key).string().not_null())
                    .col(ColumnDef::new(ScopedState::ValueJson).text().not_null())
                    .col(ColumnDef::new(ScopedState::Version).big_integer().not_null())
                    .col(
                        ColumnDef::new(ScopedState::UpdatedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ScopedState::UpdatedBy).string().null())
                    .primary_key(
                        Index::create()
                            .col(ScopedState::ScopeKey)
                            .col(ScopedState::Key),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ScopedState::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ScopedState {
    Table,
    ScopeKey,
    Key,
    ValueJson,
    Version,
    UpdatedAtMs,
    UpdatedBy,
}
