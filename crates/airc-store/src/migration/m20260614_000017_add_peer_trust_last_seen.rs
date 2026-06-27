//! Seam #3.2 (IDENTITY-SCOPE-PEER-LIVENESS-MODEL) — add `last_seen_ms`
//! column to `peer_trust`.
//!
//! The trust store records WHO a peer is (pubkey), HOW MUCH we trust it
//! (tier), and WHERE to reach it (endpoints) — but not WHEN we last had
//! contact. Without a recency timestamp the substrate cannot age a peer
//! out: a friend who enrolled a year ago and a friend we exchanged a
//! beacon with this minute are indistinguishable, so liveness-based
//! eviction (`airc peer prune`'s age dimension) has nothing to read.
//!
//! This migration is the substrate column only — no update wiring, no
//! eviction policy. It mirrors how the `tier` column (card 34942ec1
//! Sub-A) landed as the dimension first; the beacon/dial touch path and
//! the age-based classifier build on top once the column exists.
//!
//! Purely additive, nullable. A NULL `last_seen_ms` is the pre-migration
//! state — the read layer floors it to `added_at_ms` (enrolment is the
//! oldest defensible "last contact"), so no backfill pass is needed and
//! a pre-migration friend never reads as instantly stale.

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
                    .add_column(ColumnDef::new(PeerTrust::LastSeenMs).big_integer().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PeerTrust::Table)
                    .drop_column(PeerTrust::LastSeenMs)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PeerTrust {
    Table,
    LastSeenMs,
}
