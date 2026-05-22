//! `local_identity` table — durable singleton metadata for this
//! airc install's stable identity.
//!
//! The secret keypair stays out of the database (see the storage
//! caveat in `airc-daemon::identity` — secret material belongs in
//! file storage with 0600 perms or, in production, behind SQLCipher
//! / OS keychain / hardware enclave). What lives here is the
//! **metadata that callers need to pair with the on-disk key**:
//! `peer_id`, `client_id`, schema version, and the originally
//! recorded creation timestamp.
//!
//! Singleton: there is exactly zero or one row. The migration uses
//! an integer primary key fixed at `1` rather than a sentinel string
//! so SQLite can short-circuit `WHERE id = 1` to an index probe.

use sea_orm::entity::prelude::*;

/// The only legal primary-key value. Encoded as `i32` so SQLite
/// stores it as INTEGER (cheap) rather than a TEXT singleton key.
pub const SINGLETON_ID: i32 = 1;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_identity")]
pub struct Model {
    /// Always [`SINGLETON_ID`]. The migration constrains it via
    /// PRIMARY KEY + an explicit CHECK so a future bug can't ever
    /// insert a second identity row.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,
    pub peer_id: Uuid,
    pub client_id: Uuid,
    pub version: i32,
    pub created_at_ms: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
