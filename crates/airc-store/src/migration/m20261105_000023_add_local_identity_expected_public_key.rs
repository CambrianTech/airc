//! Card 0f3b3126 — add `expected_public_key` custody-binding column to
//! `local_identity`.
//!
//! Purely additive: the column is a nullable BLOB, so every existing row loads
//! unchanged (binding = NULL) and no consumer code has to opt in. Mint and
//! legacy-json migration write the binding; load verifies the on-disk key file
//! against it unconditionally whenever one is present. There is deliberately no
//! backfill: deriving a public key from an existing key file would be exactly
//! the "auto-bind of an unverified file" this card forbids — pre-binding rows
//! stay unbound until re-minted or explicitly re-bound out-of-band.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(LocalIdentity::Table)
                    .add_column(
                        ColumnDef::new(LocalIdentity::ExpectedPublicKey)
                            .binary()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(LocalIdentity::Table)
                    .drop_column(LocalIdentity::ExpectedPublicKey)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum LocalIdentity {
    Table,
    ExpectedPublicKey,
}
