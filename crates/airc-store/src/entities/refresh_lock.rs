//! `refresh_locks` table — singleflight gate for rare remote registry refreshes.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "refresh_locks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub mesh_identity: String,
    pub held_at_ms: i64,
    pub holder_pid: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
