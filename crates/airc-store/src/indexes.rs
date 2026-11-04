//! The event tables' secondary indexes, built by the store's background task after a drain pass,
//! never at open (card 70055c76). On 2026-11-04 the migrations that created them took 23.4 s on a
//! 1.56 GB resident store copy, the whole of it holding the write lock, while that store was over
//! 99% rows the drain removes. So migration 000021 adds only the columns (instant in SQLite), and
//! [`ensure`] builds whichever of these is missing once the drain has made the table small.
//!
//! Until an index exists, the queries it serves still answer, by walking the table's own order:
//! correct, only slower. The pass report says which were missing ([`IndexReport`]).

use sea_orm::DatabaseConnection;
use sea_orm_migration::prelude::*;
use sea_orm_migration::SchemaManager;

use crate::error::StoreError;
use crate::sqlite::SqliteEventStore;

/// What [`ensure`] did: the indexes it found missing and built, and how long the build held the
/// file (it runs under the write gate).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexReport {
    pub built: Vec<&'static str>,
    pub held: std::time::Duration,
}

/// Every secondary index of the event tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventIndex {
    EventsRoomAddressedPeer,
    EventsRoomLeadingMention,
    BusRoomAddressedPeer,
    BusRoomLeadingMention,
    EventsDerivedVersion,
    BusDerivedVersion,
}

impl EventIndex {
    pub(crate) const ALL: [EventIndex; 6] = [
        EventIndex::EventsRoomAddressedPeer,
        EventIndex::EventsRoomLeadingMention,
        EventIndex::BusRoomAddressedPeer,
        EventIndex::BusRoomLeadingMention,
        EventIndex::EventsDerivedVersion,
        EventIndex::BusDerivedVersion,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            EventIndex::EventsRoomAddressedPeer => "idx_events_room_addressed_peer",
            EventIndex::EventsRoomLeadingMention => "idx_events_room_leading_mention",
            EventIndex::BusRoomAddressedPeer => "idx_bus_events_room_addressed_peer",
            EventIndex::BusRoomLeadingMention => "idx_bus_events_room_leading_mention",
            EventIndex::EventsDerivedVersion => "idx_events_derived_version",
            EventIndex::BusDerivedVersion => "idx_bus_events_derived_version",
        }
    }

    const fn table(self) -> &'static str {
        match self {
            EventIndex::EventsRoomAddressedPeer
            | EventIndex::EventsRoomLeadingMention
            | EventIndex::EventsDerivedVersion => "events",
            EventIndex::BusRoomAddressedPeer
            | EventIndex::BusRoomLeadingMention
            | EventIndex::BusDerivedVersion => "bus_events",
        }
    }

    fn definition(self) -> IndexCreateStatement {
        let mut index = Index::create();
        index.name(self.name()).if_not_exists();
        match self {
            // An addressee scan is `room AND (addressed_peer = me OR leading_mention)` in cursor
            // order: one index per OR arm, each in the table's cursor order.
            EventIndex::EventsRoomAddressedPeer => index
                .table(Events::Table)
                .col(Events::RoomId)
                .col(Addressing::AddressedPeer)
                .col(Events::Lamport)
                .col(Events::EventId),
            EventIndex::EventsRoomLeadingMention => index
                .table(Events::Table)
                .col(Events::RoomId)
                .col(Addressing::LeadingMention)
                .col(Events::Lamport)
                .col(Events::EventId),
            EventIndex::BusRoomAddressedPeer => index
                .table(BusEvents::Table)
                .col(BusEvents::RoomId)
                .col(Addressing::AddressedPeer)
                .col(BusEvents::Epoch)
                .col(BusEvents::Counter)
                .col(BusEvents::EventId),
            EventIndex::BusRoomLeadingMention => index
                .table(BusEvents::Table)
                .col(BusEvents::RoomId)
                .col(Addressing::LeadingMention)
                .col(BusEvents::Epoch)
                .col(BusEvents::Counter)
                .col(BusEvents::EventId),
            // A reader finds the rows its page still owes a derivation by this column.
            EventIndex::EventsDerivedVersion => {
                index.table(Events::Table).col(Addressing::DerivedVersion)
            }
            EventIndex::BusDerivedVersion => index
                .table(BusEvents::Table)
                .col(Addressing::DerivedVersion),
        };
        index.to_owned()
    }
}

/// Build whichever indexes are missing. The caller holds the file's write gate, since a build
/// holds the write lock for its whole run; on a drained table that is milliseconds.
pub(crate) async fn ensure(db: &DatabaseConnection) -> Result<IndexReport, StoreError> {
    let manager = SchemaManager::new(db);
    let started = std::time::Instant::now();
    let mut built = Vec::new();
    for index in EventIndex::ALL {
        if manager.has_index(index.table(), index.name()).await? {
            continue;
        }
        manager.create_index(index.definition()).await?;
        built.push(index.name());
    }
    Ok(IndexReport {
        built,
        held: started.elapsed(),
    })
}

/// Whether every index exists (a catalog lookup each).
pub(crate) async fn all_exist(db: &DatabaseConnection) -> Result<bool, StoreError> {
    let manager = SchemaManager::new(db);
    for index in EventIndex::ALL {
        if !manager.has_index(index.table(), index.name()).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

impl SqliteEventStore {
    /// Build the missing indexes ([`ensure`]) under the file's write gate, then mark the file
    /// indexed (`crate::settling`).
    pub async fn ensure_indexes(&self) -> Result<IndexReport, StoreError> {
        let write = self.admit_write().await?;
        let report = ensure(self.connection()).await?;
        drop(write);
        self.gate().settling.mark_indexed();
        Ok(report)
    }
}

/// Drop every secondary index that exists (migration 000021's `down`).
pub(crate) async fn drop_all(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for index in EventIndex::ALL {
        manager
            .drop_index(Index::drop().name(index.name()).if_exists().to_owned())
            .await?;
    }
    Ok(())
}

#[derive(DeriveIden)]
enum Events {
    Table,
    RoomId,
    Lamport,
    EventId,
}

#[derive(DeriveIden)]
enum BusEvents {
    Table,
    RoomId,
    Epoch,
    Counter,
    EventId,
}

#[derive(DeriveIden)]
enum Addressing {
    AddressedPeer,
    LeadingMention,
    DerivedVersion,
}
