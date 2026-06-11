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
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
