//! Add ORM-owned account-mesh presence beacons.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Beacons::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Beacons::MeshIdentity).string().not_null())
                    .col(ColumnDef::new(Beacons::PeerId).uuid().not_null())
                    .col(ColumnDef::new(Beacons::ScopeHome).string().not_null())
                    .col(ColumnDef::new(Beacons::Pid).big_integer().not_null())
                    .col(
                        ColumnDef::new(Beacons::PublishedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Beacons::HeartbeatAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Beacons::MeshIdentity)
                            .col(Beacons::PeerId),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(BeaconChannels::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BeaconChannels::MeshIdentity)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(BeaconChannels::PeerId).uuid().not_null())
                    .col(
                        ColumnDef::new(BeaconChannels::ChannelName)
                            .string()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(BeaconChannels::MeshIdentity)
                            .col(BeaconChannels::PeerId)
                            .col(BeaconChannels::ChannelName),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_beacons_mesh_heartbeat")
                    .table(Beacons::Table)
                    .col(Beacons::MeshIdentity)
                    .col(Beacons::HeartbeatAtMs)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BeaconChannels::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Beacons::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Beacons {
    Table,
    MeshIdentity,
    PeerId,
    ScopeHome,
    Pid,
    PublishedAtMs,
    HeartbeatAtMs,
}

#[derive(DeriveIden)]
enum BeaconChannels {
    Table,
    MeshIdentity,
    PeerId,
    ChannelName,
}
