//! Add ORM-owned peer trust and rotation audit tables.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PeerTrust::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PeerTrust::PeerId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PeerTrust::PubkeyB64).string().not_null())
                    .col(
                        ColumnDef::new(PeerTrust::AddedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(PeerRotationAudit::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(PeerRotationAudit::PeerId).uuid().not_null())
                    .col(
                        ColumnDef::new(PeerRotationAudit::Sequence)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PeerRotationAudit::PrevPubkeyB64)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PeerRotationAudit::NextPubkeyB64)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PeerRotationAudit::RotatedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PeerRotationAudit::AppliedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(PeerRotationAudit::PeerId)
                            .col(PeerRotationAudit::Sequence),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_peer_rotation_audit_peer_sequence")
                    .table(PeerRotationAudit::Table)
                    .col(PeerRotationAudit::PeerId)
                    .col(PeerRotationAudit::Sequence)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PeerRotationAudit::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(PeerTrust::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum PeerTrust {
    Table,
    PeerId,
    PubkeyB64,
    AddedAtMs,
}

#[derive(DeriveIden)]
enum PeerRotationAudit {
    Table,
    PeerId,
    Sequence,
    PrevPubkeyB64,
    NextPubkeyB64,
    RotatedAtMs,
    AppliedAtMs,
}
