//! Card 34942ec1 Sub-A — add `tier` discriminator column to
//! `peer_trust`.
//!
//! Purely additive. The column is `TEXT NOT NULL DEFAULT 'untrusted'`,
//! so every existing row gets the `"untrusted"` tier without touching
//! its other fields, and no consumer code has to opt in to the new
//! column to keep working.
//!
//! The wire strings come from [`crate::peer_trust::TrustTier::as_wire_str`];
//! the migration deliberately uses the literal "untrusted" rather than
//! importing the enum because migration files are pinned at the
//! schema state they were authored against — a future refactor of
//! the enum mustn't change what gets written into existing
//! databases at re-migration time.
//!
//! Sub-B (detection: OwnMachine via UDS sibling, OwnAccount via mesh
//! identity) and Sub-C (consumer-side policy gates) build on this
//! column without further schema work.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PeerTrust::Table)
                    .add_column(
                        ColumnDef::new(PeerTrust::Tier)
                            .string()
                            .not_null()
                            .default("untrusted"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PeerTrust::Table)
                    .drop_column(PeerTrust::Tier)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PeerTrust {
    Table,
    Tier,
}
