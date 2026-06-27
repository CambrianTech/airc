//! `peer_rotation_audit` table — append-only peer key rotations.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "peer_rotation_audit")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub peer_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub sequence: i64,
    pub prev_pubkey_b64: String,
    pub next_pubkey_b64: String,
    pub rotated_at_ms: i64,
    pub applied_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
