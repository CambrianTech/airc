//! Initial migration: create the `events` table + composite indexes
//! to keep `page_recent` and `resume_from` cheap.
//!
//! Index strategy:
//!   - `idx_events_lamport_event_id` covers the global ordering used
//!     by `page_recent(channel = None)` and `resume_from(channel = None)`.
//!   - `idx_events_room_lamport_event_id` covers the per-room cases
//!     `page_recent(channel = Some(_))` and `resume_from(channel = Some(_))`.
//!
//! Both indexes lead with the filter column (room_id when present)
//! so SQLite's query planner does a single B-tree range scan rather
//! than scanning the whole table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Events::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Events::EventId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Events::RoomId).uuid().not_null())
                    .col(ColumnDef::new(Events::PeerId).uuid().not_null())
                    .col(ColumnDef::new(Events::ClientId).uuid().not_null())
                    .col(ColumnDef::new(Events::Kind).string().not_null())
                    .col(
                        ColumnDef::new(Events::OccurredAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Events::Lamport).big_integer().not_null())
                    .col(ColumnDef::new(Events::Target).json().not_null())
                    .col(ColumnDef::new(Events::Headers).json().not_null())
                    .col(ColumnDef::new(Events::Body).json())
                    .col(ColumnDef::new(Events::Attachment).json())
                    .col(ColumnDef::new(Events::Receipt).json())
                    .col(ColumnDef::new(Events::Metadata).json().not_null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_events_lamport_event_id")
                    .table(Events::Table)
                    .col(Events::Lamport)
                    .col(Events::EventId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_events_room_lamport_event_id")
                    .table(Events::Table)
                    .col(Events::RoomId)
                    .col(Events::Lamport)
                    .col(Events::EventId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Events::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Events {
    Table,
    EventId,
    RoomId,
    PeerId,
    ClientId,
    Kind,
    OccurredAtMs,
    Lamport,
    Target,
    Headers,
    Body,
    Attachment,
    Receipt,
    Metadata,
}
