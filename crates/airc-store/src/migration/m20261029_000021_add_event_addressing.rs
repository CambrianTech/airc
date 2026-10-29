//! Index who each stored event could be addressed to (card 6fb2ea16).
//!
//! An agent's "requests addressed to you" search scanned bounded tails and
//! filtered in Rust, so under sustained ambient traffic an older request was
//! never reached. Both event tables gain the two derived columns an addressee
//! scan selects on, [`airc_core::Addressing`]: `addressed_peer` (the peer a
//! typed target names) and `leading_mention` (the body text opens with an
//! `@name`). The store writes them at append; the stored event and the signed
//! envelope bytes are untouched.
//!
//! A third column, `derived_version`, marks rows whose derived data
//! (addressing, and the header rows of migration 000022) is current. This
//! migration is DDL only and runs, with its version row, inside the one
//! transaction `apply_migrations` wraps around `Migrator::up`; the derivation
//! itself is `crate::derive`, resumable and run after every open, so rows an
//! interrupted backfill or an older writer left at version 0 are healed.

use sea_orm_migration::prelude::*;

use super::fault::{self, Fault};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [Events::Table.into_iden(), BusEvents::Table.into_iden()] {
            manager
                .alter_table(
                    Table::alter()
                        .table(table.clone())
                        .add_column(ColumnDef::new(Addressing::AddressedPeer).uuid().null())
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(table.clone())
                        .add_column(
                            ColumnDef::new(Addressing::LeadingMention)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
            // 0 = not derived: every existing row, and any row an older writer
            // inserts without the column.
            manager
                .alter_table(
                    Table::alter()
                        .table(table)
                        .add_column(
                            ColumnDef::new(Addressing::DerivedVersion)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }
        // An addressee scan is `room AND (addressed_peer = me OR leading_mention)`
        // in cursor order: one index per OR arm, each in the table's cursor order.
        for (name, column) in [
            ("idx_events_room_addressed_peer", Addressing::AddressedPeer),
            (
                "idx_events_room_leading_mention",
                Addressing::LeadingMention,
            ),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(Events::Table)
                        .col(Events::RoomId)
                        .col(column)
                        .col(Events::Lamport)
                        .col(Events::EventId)
                        .to_owned(),
                )
                .await?;
        }
        for (name, column) in [
            (
                "idx_bus_events_room_addressed_peer",
                Addressing::AddressedPeer,
            ),
            (
                "idx_bus_events_room_leading_mention",
                Addressing::LeadingMention,
            ),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(BusEvents::Table)
                        .col(BusEvents::RoomId)
                        .col(column)
                        .col(BusEvents::Epoch)
                        .col(BusEvents::Counter)
                        .col(BusEvents::EventId)
                        .to_owned(),
                )
                .await?;
        }
        // The derivation finds the rows it still owes by this column.
        for (name, table) in [
            ("idx_events_derived_version", Events::Table.into_iden()),
            (
                "idx_bus_events_derived_version",
                BusEvents::Table.into_iden(),
            ),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(table)
                        .col(Addressing::DerivedVersion)
                        .to_owned(),
                )
                .await?;
        }
        fault::hit(Fault::AfterAddressingDdl)
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for name in [
            "idx_events_room_addressed_peer",
            "idx_events_room_leading_mention",
            "idx_bus_events_room_addressed_peer",
            "idx_bus_events_room_leading_mention",
            "idx_events_derived_version",
            "idx_bus_events_derived_version",
        ] {
            manager
                .drop_index(Index::drop().name(name).to_owned())
                .await?;
        }
        // SQLite takes one alter option per statement.
        for table in [Events::Table.into_iden(), BusEvents::Table.into_iden()] {
            for column in [
                Addressing::AddressedPeer,
                Addressing::LeadingMention,
                Addressing::DerivedVersion,
            ] {
                manager
                    .alter_table(
                        Table::alter()
                            .table(table.clone())
                            .drop_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }
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
