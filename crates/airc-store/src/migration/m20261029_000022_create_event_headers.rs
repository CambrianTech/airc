//! Create `event_headers` and `bus_event_headers`: one row per header of an
//! `events` / `bus_events` row, so header predicates are typed subqueries on an
//! entity (`crate::header_query`) instead of `json_each` SQL over the headers
//! JSON. The JSON column and the signed envelope bytes stay as they are; these
//! tables are a derived, queryable copy the store writes at append.
//!
//! Existing rows are backfilled through the entities, a keyset page at a time:
//! read `(event_id, headers)`, decode the headers in Rust, insert their rows.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use sea_orm_migration::prelude::*;
use uuid::Uuid;

use crate::entities::{bus_event, bus_event_header, event, event_header};
use crate::header_query::{bus_event_header_rows, event_header_rows, HEADER_ROWS_PER_INSERT};

/// Owner rows read per backfill page.
const PAGE: u64 = 1000;

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
        let db = manager.get_connection();
        backfill_events(db).await?;
        backfill_bus_events(db).await
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

fn decode(what: &str, error: impl std::fmt::Display) -> DbErr {
    DbErr::Custom(format!("header backfill: {what}: {error}"))
}

async fn backfill_events(db: &SchemaManagerConnection<'_>) -> Result<(), DbErr> {
    let mut last: Option<Uuid> = None;
    loop {
        let mut page = event::Entity::find()
            .select_only()
            .column(event::Column::EventId)
            .column(event::Column::Headers)
            .order_by_asc(event::Column::EventId)
            .limit(PAGE);
        if let Some(after) = last {
            page = page.filter(event::Column::EventId.gt(after));
        }
        let rows: Vec<(Uuid, serde_json::Value)> = page.into_tuple().all(db).await?;
        let Some((tail, _)) = rows.last() else {
            return Ok(());
        };
        last = Some(*tail);
        let mut headers = Vec::new();
        for (id, json) in rows {
            let held: airc_core::Headers =
                serde_json::from_value(json).map_err(|e| decode("events.headers", e))?;
            headers.extend(event_header_rows(id, &held));
        }
        for chunk in headers.chunks(HEADER_ROWS_PER_INSERT) {
            match event_header::Entity::insert_many(chunk.to_vec())
                .on_conflict(
                    OnConflict::columns([event_header::Column::EventId, event_header::Column::Key])
                        .do_nothing()
                        .to_owned(),
                )
                .exec(db)
                .await
            {
                Ok(_) | Err(DbErr::RecordNotInserted) => {}
                Err(err) => return Err(err),
            }
        }
    }
}

async fn backfill_bus_events(db: &SchemaManagerConnection<'_>) -> Result<(), DbErr> {
    let mut last: Option<Uuid> = None;
    loop {
        let mut page = bus_event::Entity::find()
            .select_only()
            .column(bus_event::Column::EventId)
            .column(bus_event::Column::Headers)
            .order_by_asc(bus_event::Column::EventId)
            .limit(PAGE);
        if let Some(after) = last {
            page = page.filter(bus_event::Column::EventId.gt(after));
        }
        let rows: Vec<(Uuid, serde_json::Value)> = page.into_tuple().all(db).await?;
        let Some((tail, _)) = rows.last() else {
            return Ok(());
        };
        last = Some(*tail);
        let mut headers = Vec::new();
        for (id, json) in rows {
            let held: airc_core::Headers =
                serde_json::from_value(json).map_err(|e| decode("bus_events.headers", e))?;
            headers.extend(bus_event_header_rows(id, &held));
        }
        for chunk in headers.chunks(HEADER_ROWS_PER_INSERT) {
            match bus_event_header::Entity::insert_many(chunk.to_vec())
                .on_conflict(
                    OnConflict::columns([
                        bus_event_header::Column::EventId,
                        bus_event_header::Column::Key,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec(db)
                .await
            {
                Ok(_) | Err(DbErr::RecordNotInserted) => {}
                Err(err) => return Err(err),
            }
        }
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
