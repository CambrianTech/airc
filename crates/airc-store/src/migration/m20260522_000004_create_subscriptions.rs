//! Add ORM-owned subscription/default-channel state.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Subscriptions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Subscriptions::ChannelName)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Subscriptions::RoomId).uuid().not_null())
                    .col(ColumnDef::new(Subscriptions::Wire).string().not_null())
                    .col(
                        ColumnDef::new(Subscriptions::JoinedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Subscriptions::IsDefault)
                            .boolean()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Subscriptions::Parted).boolean().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_subscriptions_default")
                    .table(Subscriptions::Table)
                    .col(Subscriptions::IsDefault)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Subscriptions::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Subscriptions {
    Table,
    ChannelName,
    RoomId,
    Wire,
    JoinedAtMs,
    IsDefault,
    Parted,
}
