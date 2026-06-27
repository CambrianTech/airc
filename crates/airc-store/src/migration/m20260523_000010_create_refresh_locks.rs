//! Add ORM-owned remote-refresh singleflight locks.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RefreshLocks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RefreshLocks::MeshIdentity)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(RefreshLocks::HeldAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RefreshLocks::HolderPid)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RefreshLocks::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RefreshLocks {
    Table,
    MeshIdentity,
    HeldAtMs,
    HolderPid,
}
