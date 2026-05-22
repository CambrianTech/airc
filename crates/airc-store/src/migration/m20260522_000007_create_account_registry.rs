//! Add ORM-owned account registry document cache + per-mesh-identity
//! gh-gist sentinel table.
//!
//! Pairs the two tables in a single migration because they're the
//! same feature surface (account_registry storage) and we don't
//! anticipate adding one without the other.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AccountRegistry::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AccountRegistry::MeshIdentity)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AccountRegistry::SchemaVersion)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AccountRegistry::GeneratedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AccountRegistry::DocumentJson)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AccountRegistry::UpdatedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(AccountRegistryGistSentinel::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AccountRegistryGistSentinel::MeshIdentity)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AccountRegistryGistSentinel::GistId)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AccountRegistryGistSentinel::UpdatedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(AccountRegistryGistSentinel::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AccountRegistry::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AccountRegistry {
    Table,
    MeshIdentity,
    SchemaVersion,
    GeneratedAtMs,
    DocumentJson,
    UpdatedAtMs,
}

#[derive(DeriveIden)]
enum AccountRegistryGistSentinel {
    Table,
    MeshIdentity,
    GistId,
    UpdatedAtMs,
}
