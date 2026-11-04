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
//! migration adds the columns only, which SQLite does without touching a row,
//! and runs with its version row inside the one transaction `apply_migrations`
//! wraps around `Migrator::up`. Nothing here reads or writes rows: a reader
//! derives the rows of the page it returns (`crate::derive`), and the indexes
//! are built by the store's background task after its drain ([`crate::indexes`],
//! card 70055c76: building them here held a 1.56 GB store's write lock 23.4 s).

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
        fault::hit(Fault::AfterAddressingDdl)
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        crate::indexes::drop_all(manager).await?;
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
}

#[derive(DeriveIden)]
enum BusEvents {
    Table,
}

#[derive(DeriveIden)]
enum Addressing {
    AddressedPeer,
    LeadingMention,
    DerivedVersion,
}
