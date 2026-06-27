//! Channels attached to account-mesh presence beacons.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "beacon_channels")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub mesh_identity: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub peer_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub channel_name: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
