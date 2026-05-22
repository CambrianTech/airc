//! Add the singleton `local_identity` metadata table.
//!
//! Pairs with the on-disk `identity.key` file kept by
//! `airc-daemon::identity`. Secret material (the 32-byte Ed25519
//! seed) stays out of the database; this table holds the metadata
//! (`peer_id`, `client_id`, schema version, creation timestamp) the
//! daemon used to keep in `identity.json`.
//!
//! Singleton invariant enforced two ways:
//! 1. PRIMARY KEY constrains the row count via UNIQUE.
//! 2. CHECK (id = 1) blocks any caller from inserting a different
//!    sentinel even if a future bug forgets the
//!    `local_identity::SINGLETON_ID` constant.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(LocalIdentity::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LocalIdentity::Id)
                            .integer()
                            .not_null()
                            .primary_key()
                            .check(Expr::col(LocalIdentity::Id).eq(1)),
                    )
                    .col(ColumnDef::new(LocalIdentity::PeerId).uuid().not_null())
                    .col(ColumnDef::new(LocalIdentity::ClientId).uuid().not_null())
                    .col(ColumnDef::new(LocalIdentity::Version).integer().not_null())
                    .col(
                        ColumnDef::new(LocalIdentity::CreatedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(LocalIdentity::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum LocalIdentity {
    Table,
    Id,
    PeerId,
    ClientId,
    Version,
    CreatedAtMs,
}
