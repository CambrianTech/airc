//! `runtime_cursors` table — durable consumer checkpoints.
//!
//! A runtime cursor is not configuration. It is substrate state: a
//! named consumer's last acknowledged transcript position. Hooks,
//! joins, monitors, and future Continuum/OpenClaw/Hermes consumers use
//! this table instead of sidecar JSON cursor files.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "runtime_cursors")]
pub struct Model {
    /// Stable consumer identifier, e.g. `join-feed:codex:<thread>` or
    /// `codex-hook:default`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub consumer_id: String,
    pub lamport: i64,
    pub event_id: Uuid,
    pub updated_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
