//! Create the `bus_events` table — the owner-core durable tier
//! (§3.3 ORM durable tier of `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! One row per persisted `airc_bus::Envelope`. This is a CLEAN schema
//! for the generic envelope — deliberately NOT `TranscriptEvent`
//! (`Envelope != TranscriptEvent`): opaque `payload` BLOB, generational
//! `seq = (epoch, counter)`, `Target`/`Kind`/`DeliveryClass` as text.
//!
//! Index strategy:
//!   - `idx_bus_events_room_epoch_counter_event_id` covers the
//!     generational cursor order `(room_id, epoch, counter, event_id)`
//!     used by `DurableSink::page` — the §3.5 bounded deep-replay. It is
//!     NOT a single lamport: `epoch` leads `counter` so a post-crash
//!     event sorts strictly after a pre-crash one (§3.8 crash-safe
//!     order). Leading with `room_id` lets SQLite's planner do one
//!     B-tree range scan per channel rather than a full-table scan.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(BusEvents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BusEvents::EventId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(BusEvents::RoomId).uuid().not_null())
                    .col(ColumnDef::new(BusEvents::Epoch).big_integer().not_null())
                    .col(ColumnDef::new(BusEvents::Counter).big_integer().not_null())
                    .col(ColumnDef::new(BusEvents::Kind).string().not_null())
                    .col(ColumnDef::new(BusEvents::Delivery).string().not_null())
                    .col(ColumnDef::new(BusEvents::Target).json().not_null())
                    .col(ColumnDef::new(BusEvents::CorrelationId).uuid().null())
                    .col(ColumnDef::new(BusEvents::CoalesceKey).string().null())
                    .col(ColumnDef::new(BusEvents::Headers).json().not_null())
                    .col(ColumnDef::new(BusEvents::Payload).blob().not_null())
                    .col(ColumnDef::new(BusEvents::PeerId).uuid().not_null())
                    .col(ColumnDef::new(BusEvents::ClientId).uuid().not_null())
                    .col(
                        ColumnDef::new(BusEvents::OccurredAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_bus_events_room_epoch_counter_event_id")
                    .table(BusEvents::Table)
                    .col(BusEvents::RoomId)
                    .col(BusEvents::Epoch)
                    .col(BusEvents::Counter)
                    .col(BusEvents::EventId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BusEvents::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BusEvents {
    Table,
    EventId,
    RoomId,
    Epoch,
    Counter,
    Kind,
    Delivery,
    Target,
    CorrelationId,
    CoalesceKey,
    Headers,
    Payload,
    PeerId,
    ClientId,
    OccurredAtMs,
}
