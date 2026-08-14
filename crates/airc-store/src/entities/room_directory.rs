//! `room_directory` table — the account's `label → RoomId` index.
//!
//! The ONLY place a room label is a key. Everywhere else the label is a
//! display string and the `RoomId` is the address.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "room_directory")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub label: String,
    pub room_id: Uuid,
    pub claimed_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
