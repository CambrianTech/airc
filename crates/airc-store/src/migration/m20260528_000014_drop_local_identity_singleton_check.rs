//! Card 8384cc18 Sub-B — drop the singleton `CHECK (id = 1)` from
//! `local_identity`.
//!
//! SQLite cannot drop a CHECK constraint in-place, so this migration
//! recreates the table with the same columns, copies every row across,
//! and drops the legacy table. The existing row keeps `id = 1` and
//! `agent_name = 'default'`; after this migration, additional rows
//! with distinct ids and agent names are legal.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const AGENT_NAME_INDEX: &str = "idx_local_identity_agent_name";
const LEGACY_TABLE: &str = "local_identity_legacy";

const COPY_COLUMNS: [LocalIdentity; 13] = [
    LocalIdentity::Id,
    LocalIdentity::PeerId,
    LocalIdentity::ClientId,
    LocalIdentity::Version,
    LocalIdentity::CreatedAtMs,
    LocalIdentity::Name,
    LocalIdentity::Pronouns,
    LocalIdentity::Role,
    LocalIdentity::Bio,
    LocalIdentity::Status,
    LocalIdentity::Fingerprint,
    LocalIdentity::IntegrationsJson,
    LocalIdentity::AgentName,
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .rename_table(
                Table::rename()
                    .table(LocalIdentity::Table, Alias::new(LEGACY_TABLE))
                    .to_owned(),
            )
            .await?;
        manager.create_table(local_identity_table(false)).await?;
        copy_rows(manager, false).await?;
        manager
            .drop_table(Table::drop().table(Alias::new(LEGACY_TABLE)).to_owned())
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(AGENT_NAME_INDEX)
                    .table(LocalIdentity::Table)
                    .col(LocalIdentity::AgentName)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(AGENT_NAME_INDEX)
                    .table(LocalIdentity::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .rename_table(
                Table::rename()
                    .table(LocalIdentity::Table, Alias::new(LEGACY_TABLE))
                    .to_owned(),
            )
            .await?;
        manager.create_table(local_identity_table(true)).await?;
        copy_rows(manager, true).await?;
        manager
            .drop_table(Table::drop().table(Alias::new(LEGACY_TABLE)).to_owned())
            .await
    }
}

fn local_identity_table(with_singleton_check: bool) -> TableCreateStatement {
    let mut id = ColumnDef::new(LocalIdentity::Id);
    id.integer().not_null().primary_key();
    if with_singleton_check {
        id.check(Expr::col(LocalIdentity::Id).eq(1));
    }

    Table::create()
        .table(LocalIdentity::Table)
        .col(id)
        .col(ColumnDef::new(LocalIdentity::PeerId).uuid().not_null())
        .col(ColumnDef::new(LocalIdentity::ClientId).uuid().not_null())
        .col(ColumnDef::new(LocalIdentity::Version).integer().not_null())
        .col(
            ColumnDef::new(LocalIdentity::CreatedAtMs)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(LocalIdentity::Name)
                .string()
                .not_null()
                .default(""),
        )
        .col(
            ColumnDef::new(LocalIdentity::Pronouns)
                .string()
                .not_null()
                .default(""),
        )
        .col(
            ColumnDef::new(LocalIdentity::Role)
                .string()
                .not_null()
                .default(""),
        )
        .col(
            ColumnDef::new(LocalIdentity::Bio)
                .string()
                .not_null()
                .default(""),
        )
        .col(
            ColumnDef::new(LocalIdentity::Status)
                .string()
                .not_null()
                .default(""),
        )
        .col(
            ColumnDef::new(LocalIdentity::Fingerprint)
                .string()
                .not_null()
                .default(""),
        )
        .col(
            ColumnDef::new(LocalIdentity::IntegrationsJson)
                .json()
                .not_null()
                .default("{}"),
        )
        .col(
            ColumnDef::new(LocalIdentity::AgentName)
                .string()
                .not_null()
                .default("default"),
        )
        .to_owned()
}

async fn copy_rows(manager: &SchemaManager<'_>, singleton_only: bool) -> Result<(), DbErr> {
    let mut select = Query::select();
    select.columns(COPY_COLUMNS).from(Alias::new(LEGACY_TABLE));
    if singleton_only {
        select.and_where(Expr::col(LocalIdentity::Id).eq(1));
    }

    let mut insert = Query::insert();
    insert
        .into_table(LocalIdentity::Table)
        .columns(COPY_COLUMNS);
    insert
        .select_from(select.to_owned())
        .map_err(|err| DbErr::Migration(err.to_string()))?;
    manager.exec_stmt(insert.to_owned()).await
}

#[derive(DeriveIden, Copy, Clone)]
enum LocalIdentity {
    Table,
    Id,
    PeerId,
    ClientId,
    Version,
    CreatedAtMs,
    Name,
    Pronouns,
    Role,
    Bio,
    Status,
    Fingerprint,
    IntegrationsJson,
    AgentName,
}
