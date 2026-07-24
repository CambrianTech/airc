//! Self-healing join (machine-vs-scope cert identity) — add
//! `endpoints_peer_id` to `peer_trust`.
//!
//! Live two-machine evidence: advertised endpoints answer TLS with the
//! MACHINE (daemon) identity while scopes send messages as SCOPE peers.
//! A dial that cert-pins the scope peer therefore fails with a loud
//! identity mismatch, and a human recovers by redialing pinned to the
//! machine id. This column records that machine↔scope mapping on the
//! trust record — the peer id of the transport host whose TLS cert
//! actually answers at `endpoints_json` — so a dialer can pin correctly
//! the FIRST time. Cert pinning stays strict: the dial layer only
//! honors a mapping whose host identity is itself enrolled.
//!
//! NOTE: distinct from the mesh-identity "machine-id" (a registry
//! rendezvous key string). This is a cert (keypair) identity.
//!
//! Purely additive, nullable. NULL = the endpoints answer as the row's
//! own peer (single-identity and pre-mapping records) — exactly the
//! pre-migration behavior, so no backfill pass is needed.

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
                    .add_column(ColumnDef::new(PeerTrust::EndpointsPeerId).uuid().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PeerTrust::Table)
                    .drop_column(PeerTrust::EndpointsPeerId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PeerTrust {
    Table,
    EndpointsPeerId,
}
