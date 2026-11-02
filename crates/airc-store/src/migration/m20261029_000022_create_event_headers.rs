//! Create `event_headers` and `bus_event_headers`: one row per header of an
//! `events` / `bus_events` row, so header predicates are typed subqueries on an
//! entity (`crate::header_query`) instead of `json_each` SQL over the headers
//! JSON. The JSON column and the signed envelope bytes stay as they are; these
//! tables are a derived, queryable copy the store writes at append.
//!
//! DDL only, inside `apply_migrations`' one transaction; existing rows get
//! their header rows from `crate::derive` (they are at `derived_version` 0).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (table, index) in [
            (
                EventHeaders::Table.into_iden(),
                "idx_event_headers_key_value",
            ),
            (
                BusEventHeaders::Table.into_iden(),
                "idx_bus_event_headers_key_value",
            ),
        ] {
            manager
                .create_table(
                    Table::create()
                        .table(table.clone())
                        .if_not_exists()
                        .col(ColumnDef::new(Header::EventId).uuid().not_null())
                        .col(ColumnDef::new(Header::Key).string().not_null())
                        .col(ColumnDef::new(Header::Value).string().not_null())
                        .primary_key(Index::create().col(Header::EventId).col(Header::Key))
                        .to_owned(),
                )
                .await?;
            // A predicate looks up by key (and value), returning event ids.
            manager
                .create_index(
                    Index::create()
                        .name(index)
                        .table(table)
                        .col(Header::Key)
                        .col(Header::Value)
                        .col(Header::EventId)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            EventHeaders::Table.into_iden(),
            BusEventHeaders::Table.into_iden(),
        ] {
            manager
                .drop_table(Table::drop().table(table).to_owned())
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum EventHeaders {
    Table,
}

#[derive(DeriveIden)]
enum BusEventHeaders {
    Table,
}

#[derive(DeriveIden)]
enum Header {
    EventId,
    Key,
    Value,
}
