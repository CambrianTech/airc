//! `event_headers` table — one row per header of the transcript `events` row with the same
//! `event_id`: a derived, queryable copy written beside the row at append
//! (the stored headers JSON and any signed envelope bytes are untouched), so a
//! [`airc_core::HeaderFilter`] is evaluated as typed subqueries on this entity
//! (`crate::header_query`), never by parsing JSON in SQL.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "event_headers")]
pub struct Model {
    /// The `events` row this header belongs to.
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub value: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
