//! `scoped_state` table — the generic scoped key→JSON store.
//!
//! Domain doc: [`crate::scoped_state`]. Composite primary key
//! `(scope_key, key)`; `value_json` is opaque TEXT the store never
//! parses; `updated_by` is nullable (system / anonymous writes leave it
//! unset). The leftmost-prefix of the composite PK index serves
//! `list_scoped_state(scope_key)` as a range scan, so no secondary
//! index is needed.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "scoped_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_key: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub value_json: String,
    pub version: i64,
    pub updated_at_ms: i64,
    pub updated_by: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
