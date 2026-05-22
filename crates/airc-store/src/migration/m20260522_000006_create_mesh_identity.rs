//! Add ORM-owned mesh identity cache.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(MeshIdentity::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MeshIdentity::Scope)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(MeshIdentity::Identity).string().not_null())
                    .col(ColumnDef::new(MeshIdentity::Source).string().not_null())
                    .col(
                        ColumnDef::new(MeshIdentity::ResolvedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(MeshIdentity::TtlMs).big_integer().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(MeshIdentity::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum MeshIdentity {
    Table,
    Scope,
    Identity,
    Source,
    ResolvedAtMs,
    TtlMs,
}
