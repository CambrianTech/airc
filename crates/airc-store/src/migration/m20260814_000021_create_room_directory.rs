//! The room directory — the account's `label → RoomId` index.
//!
//! Room ids are MINTED, never derived from a name, so two scopes on one
//! account that each `airc join general` would otherwise mint two rooms
//! and never see each other. This table is where they meet: the first
//! scope to use a label claims it with the id it minted, and every later
//! scope reads that id back and JOINS it.
//!
//! It is a discovery index, not an identity: the label is the key here
//! and NOWHERE else, the id it maps to is the room's only address, and a
//! room that never went through a label (dispatched into, handed over by
//! a peer) is simply absent from this table and perfectly addressable.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RoomDirectory::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RoomDirectory::Label)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(RoomDirectory::RoomId).uuid().not_null())
                    .col(
                        ColumnDef::new(RoomDirectory::ClaimedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RoomDirectory::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RoomDirectory {
    Table,
    Label,
    RoomId,
    ClaimedAtMs,
}
