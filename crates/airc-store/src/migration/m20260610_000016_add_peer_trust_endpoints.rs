//! Card 625abe6d slice 1 — add `endpoints_json` column to
//! `peer_trust`.
//!
//! Purely additive, nullable. A peer record without endpoints is the
//! pre-slice-1 state (identity-only enrolment); the route resolver
//! treats NULL exactly like an empty endpoint list — no dial
//! candidates from this record. Endpoints arrive via account-registry
//! import (card e3ebce7a's rung 1), mDNS announce (rung 2, future),
//! or the dev verb `airc peer add --endpoint`.
//!
//! The column stores the serde JSON of `Vec<RouteEndpoint>` (defined
//! in airc-lib). It is an opaque string at this layer on purpose:
//! airc-store sits below airc-lib in the dependency graph and must
//! not know transport endpoint variants. Typed encode/decode lives
//! with the enum.

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
                    .add_column(ColumnDef::new(PeerTrust::EndpointsJson).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PeerTrust::Table)
                    .drop_column(PeerTrust::EndpointsJson)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PeerTrust {
    Table,
    EndpointsJson,
}
