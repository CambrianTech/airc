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
//! (since card 8384cc18 Sub-A) an `agent_name` discriminator.
//!
//! Singleton today, multi-agent target: the migration still uses an
//! integer primary key fixed at `1` (CHECK constraint enforced),
//! so a fresh DB has exactly zero or one row. Card 8384cc18 Sub-A
//! adds `agent_name TEXT NOT NULL DEFAULT 'default'` purely
//! additively — every existing row gets named `"default"`. Sub-B
//! drops the CHECK + table-recreates so multiple rows become legal;
//! Sub-C surfaces the agent-name read API; Sub-D wires
//! `AIRC_AGENT_ID` / `airc init --as <name>`.
//!
//! [`SINGLETON_ID`] stays the documented primary-key value until
//! Sub-B lands.

use sea_orm::entity::prelude::*;

/// The only legal primary-key value while the singleton CHECK is in
/// place (card 8384cc18 Sub-A → Sub-B will drop it). Encoded as `i32`
/// so SQLite stores it as INTEGER (cheap) rather than a TEXT
/// singleton key.
pub const SINGLETON_ID: i32 = 1;

/// The default agent name used for every pre-card-8384cc18 row and
/// every fresh row created before `airc init --as <name>` ships
/// (Sub-D). Pinned as a constant so callers can write the
/// "default-agent" name once and have it propagate.
pub const DEFAULT_AGENT_NAME: &str = "default";

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "local_identity")]
pub struct Model {
    /// Currently always [`SINGLETON_ID`] (migration enforces it via
    /// CHECK). Card 8384cc18 Sub-B drops the CHECK to allow multiple
    /// rows; at that point `agent_name` becomes the natural read-key
    /// and `id` becomes a non-meaningful row sequence number.
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
