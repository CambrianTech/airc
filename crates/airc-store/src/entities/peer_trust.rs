//! `peer_trust` table — enrolled peer trust anchors.
//!
//! A peer's public key is durable substrate state. It belongs in the
//! store next to transcript/replay state, not in ad-hoc JSON files.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "peer_trust")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub peer_id: Uuid,
    pub pubkey_b64: String,
    pub added_at_ms: i64,
    /// Card 34942ec1 Sub-A: trust gradient column.
    /// Wire-string of [`crate::peer_trust::TrustTier`]. Default
    /// "untrusted" applied at migration time so pre-Sub-A rows
    /// keep working without a backfill pass.
    #[sea_orm(default_value = "untrusted")]
    pub tier: String,
    /// Card 625abe6d slice 1: serde JSON of the peer's advertised
    /// `Vec<RouteEndpoint>` (typed at the airc-lib layer; opaque
    /// string here — store sits below lib in the dependency graph).
    /// NULL = identity-only enrolment, no dial candidates.
    pub endpoints_json: Option<String>,
    /// Seam #3.2 (liveness): epoch-ms of the last time we had contact
    /// with this peer (fresh beacon import, successful dial). NULL =
    /// never touched since enrolment; the read layer floors it to
    /// `added_at_ms` so a pre-migration row reads as last-seen-at-
    /// enrolment rather than instantly stale.
    pub last_seen_ms: Option<i64>,
    /// Self-healing join: epoch-ms freshness stamp of the CURRENT
    /// `endpoints_json` set (the advertisement instant, clamped to the
    /// importer's clock). Endpoints and stamp are written together,
    /// atomically; a write carrying a staler stamp is refused so a
    /// re-sync can never resurrect a dead `(ip, port)`. NULL =
    /// pre-migration / freshness unknown; read layer floors to 0.
    pub endpoints_advertised_at_ms: Option<i64>,
    /// Self-healing join (machine-vs-scope): the peer id of the
    /// TRANSPORT HOST whose TLS certificate answers at
    /// `endpoints_json` — the daemon (machine keypair) identity when
    /// this row is a scope peer hosted behind a shared daemon
    /// listener. Dials to this peer's endpoints must cert-pin the
    /// host, not this row's peer. Written atomically WITH the endpoint
    /// set under the same freshness stamp. NULL = the endpoints answer
    /// as this row's own peer (the pre-mapping and single-identity
    /// case). NOTE: distinct from the mesh-identity machine-id (a
    /// registry rendezvous key string) — this is a cert identity.
    pub endpoints_peer_id: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
