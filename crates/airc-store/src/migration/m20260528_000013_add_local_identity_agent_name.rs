//! Card 8384cc18 Sub-A — add `agent_name` discriminator column to
//! `local_identity`.
//!
//! Purely additive. The column is `TEXT NOT NULL DEFAULT 'default'`,
//! so every existing row gets the `"default"` agent name without
//! touching its other fields, and no consumer code has to opt in
//! to the new column to keep working.
//!
//! Why a discriminator column instead of going straight to "drop the
//! singleton CHECK"? SQLite does not natively support
//! `ALTER TABLE … DROP CONSTRAINT`, so removing the CHECK requires a
//! table-recreate (create new, INSERT … SELECT, drop old, rename).
//! That's a real migration with real failure modes; carding it
//! separately (Sub-B) keeps THIS Sub-A's blast radius to a single
//! `ALTER TABLE … ADD COLUMN`.
//!
//! After Sub-B drops the CHECK, multiple rows become possible and
//! `agent_name` becomes the natural read-key for "which agent am
//! I?" — Sub-C ships the API, Sub-D wires `AIRC_AGENT_NAME` /
//! `airc init --as <name>`.

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
                        ColumnDef::new(LocalIdentity::AgentName)
                            .string()
                            .not_null()
                            .default("default"),
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
                    .drop_column(LocalIdentity::AgentName)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum LocalIdentity {
    Table,
    AgentName,
}
