//! `local_identity` table — durable per-agent metadata for this
//! airc install's stable identity.
//!
//! The secret keypair stays out of the database (see the storage
//! caveat in `airc-daemon::identity` — secret material belongs in
//! file storage with 0600 perms or, in production, behind SQLCipher
//! / OS keychain / hardware enclave). What lives here is the
//! **metadata that callers need to pair with the on-disk key**:
//! `peer_id`, `client_id`, schema version, the originally recorded
//! creation timestamp, the user-facing identity-card fields, and
//! an `agent_name` discriminator.
//!
//! Multi-agent target: card 8384cc18 Sub-A added
//! `agent_name TEXT NOT NULL DEFAULT 'default'` additively, and
//! Sub-B table-recreated this schema to drop the former
//! `CHECK (id = 1)` singleton constraint. Multiple rows are now
//! legal at the schema layer; Sub-C surfaces the agent-name read API
//! and Sub-D wires `AIRC_AGENT_ID` / `airc init --as <name>`.
//!
//! [`SINGLETON_ID`] remains the documented primary-key value for the
//! legacy/default agent row until the higher-level APIs stop using a
//! singleton fallback.

use sea_orm::entity::prelude::*;

/// The legacy/default primary-key value. Sub-B removed the database
/// CHECK that made this the only legal id, but existing callers still
/// use it until Sub-C/Sub-D route identity lookup by agent name.
pub const SINGLETON_ID: i32 = 1;

/// The default agent name used for every pre-card-8384cc18 row and
/// every fresh row created before `airc init --as <name>` ships
/// (Sub-D). Pinned as a constant so callers can write the
/// "default-agent" name once and have it propagate.
pub const DEFAULT_AGENT_NAME: &str = "default";

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_identity")]
pub struct Model {
    /// Existing/default identities use [`SINGLETON_ID`]. Sub-B removed
    /// the singleton CHECK so additional rows can use other ids;
    /// `agent_name` is the natural read key for multi-agent callers.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,
    pub peer_id: Uuid,
    pub client_id: Uuid,
    pub version: i32,
    pub created_at_ms: i64,
    pub name: String,
    pub pronouns: String,
    pub role: String,
    pub bio: String,
    pub status: String,
    pub fingerprint: String,
    pub integrations_json: Json,
    /// Discriminator added by card 8384cc18 Sub-A. Defaults to
    /// `"default"` for every row created before this column existed;
    /// every fresh row created before Sub-D ships also takes the
    /// default. Multi-agent installs (Sub-D onward) supply
    /// distinct names; Sub-C exposes
    /// `load_local_identity_by_agent_name` so the lookup is by
    /// agent rather than by the singleton id.
    pub agent_name: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
