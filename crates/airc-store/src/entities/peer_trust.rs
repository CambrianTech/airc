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
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
