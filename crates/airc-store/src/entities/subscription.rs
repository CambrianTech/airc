//! `subscriptions` table — joined channels and default-channel state.
//!
//! Subscriptions are runtime state. They belong in the store with
//! cursors, transcript replay, and trust anchors, not in sidecar JSON.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "subscriptions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub channel_name: String,
    pub room_id: Uuid,
    pub wire: String,
    pub joined_at_ms: i64,
    pub is_default: bool,
    pub parted: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
