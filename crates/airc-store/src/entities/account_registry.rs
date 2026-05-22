//! `account_registry` + `account_registry_gist_sentinel` tables.
//!
//! The account registry document is the cross-machine snapshot of a
//! mesh-identity's published peer + channel state, signed once and
//! exchanged via the rendezvous adapter (gh-gist by default). The
//! row stored here is the **local cache** of the most recently
//! published-or-refreshed document for a given mesh identity, plus
//! the per-mesh-identity gist-id sentinel that lets the gh adapter
//! recognize "its own" gist on subsequent publishes.
//!
//! Treated as wire-payload encoding per non-negotiable #9: the
//! document body is opaque JSON (a serialized
//! `AccountRegistryDocument`) because consumers either fetch the
//! whole document or don't — there is no in-store query for "peers
//! across all registries." Live presence/peer/subscription state
//! lives in their respective dedicated tables; the registry is the
//! sync snapshot, not the source.

pub mod document {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "account_registry")]
    pub struct Model {
        /// Mesh identity string (gh login, git email, or local
        /// fallback) — the same shape used by the
        /// `subscriptions::MeshIdentity` newtype.
        #[sea_orm(primary_key, auto_increment = false)]
        pub mesh_identity: String,
        /// Schema version copied from the document at save time.
        /// Read out for the fast-path version check before parsing
        /// the body.
        pub schema_version: i32,
        /// `generated_at_ms` from the document itself (what the
        /// sender stamped). Distinct from `updated_at_ms`, which is
        /// when we last wrote the row.
        pub generated_at_ms: i64,
        /// Serialized `AccountRegistryDocument` JSON. Opaque to the
        /// store; consumers parse on read.
        pub document_json: String,
        /// Wall-clock timestamp when we last upserted this row.
        pub updated_at_ms: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod gist_sentinel {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "account_registry_gist_sentinel")]
    pub struct Model {
        /// Mesh identity this sentinel belongs to. A machine may
        /// own multiple mesh identities (work + personal gh
        /// accounts); each gets its own sentinel.
        #[sea_orm(primary_key, auto_increment = false)]
        pub mesh_identity: String,
        /// gh-gist id this machine owns for this mesh identity.
        /// Treated as opaque text by the store; the gh adapter
        /// interprets it.
        pub gist_id: String,
        /// Wall-clock timestamp when we last upserted this row.
        pub updated_at_ms: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
