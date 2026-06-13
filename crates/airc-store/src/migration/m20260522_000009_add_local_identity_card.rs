//! Add user-facing identity-card fields to the singleton local identity row.
//!
//! The keypair metadata already lives in `local_identity`; this migration
//! moves the remaining `airc identity` display fields out of `config.json`
//! and into the same ORM-owned row.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            LocalIdentity::Name,
            LocalIdentity::Pronouns,
            LocalIdentity::Role,
            LocalIdentity::Bio,
            LocalIdentity::Status,
            LocalIdentity::Fingerprint,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(LocalIdentity::Table)
                        .add_column(ColumnDef::new(column).string().not_null().default(""))
                        .to_owned(),
                )
                .await?;
        }

        manager
            .alter_table(
                Table::alter()
                    .table(LocalIdentity::Table)
                    .add_column(
                        ColumnDef::new(LocalIdentity::IntegrationsJson)
                            .json()
                            .not_null()
                            .default("{}"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in [
            LocalIdentity::IntegrationsJson,
            LocalIdentity::Fingerprint,
            LocalIdentity::Status,
            LocalIdentity::Bio,
            LocalIdentity::Role,
            LocalIdentity::Pronouns,
            LocalIdentity::Name,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(LocalIdentity::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden, Copy, Clone)]
enum LocalIdentity {
    Table,
    Name,
    Pronouns,
    Role,
    Bio,
    Status,
    Fingerprint,
    IntegrationsJson,
}
