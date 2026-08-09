//! Self-healing join (M5↔bigmama decay repro) — add
//! `endpoints_advertised_at_ms` to `peer_trust`.
//!
//! An endpoint set with no freshness stamp cannot be replaced *safely*:
//! the reader has no way to tell a fresh advertisement from a stale one,
//! so any merge/import ordering bug silently resurrects a dead
//! `(ip, port)` (the live repro: a re-sync took a peer's new IP but kept
//! its stale port, dialing .249:58842 forever while the daemon listened
//! on .249:57958). This column stamps the endpoint SET with the
//! advertisement instant so the store can enforce "a fresher
//! advertisement fully replaces the older endpoints; a staler one is
//! refused" — endpoints and their stamp move together, atomically.
//!
//! Purely additive, nullable. A NULL stamp is the pre-migration state —
//! the read layer floors it to 0 ("freshness unknown / epoch"), so any
//! stamped advertisement outranks a legacy unstamped set and no
//! backfill pass is needed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PeerTrust::Table)
                    .add_column(
                        ColumnDef::new(PeerTrust::EndpointsAdvertisedAtMs)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PeerTrust::Table)
                    .drop_column(PeerTrust::EndpointsAdvertisedAtMs)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PeerTrust {
    Table,
    EndpointsAdvertisedAtMs,
}
