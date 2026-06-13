//! Account-mesh presence beacon tables.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "beacons")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub mesh_identity: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub peer_id: Uuid,
    pub scope_home: String,
    pub pid: i64,
    pub published_at_ms: i64,
    pub heartbeat_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
