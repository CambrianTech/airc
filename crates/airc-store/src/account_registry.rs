//! Public DTOs for the `account_registry` + `account_registry_gist_sentinel`
//! tables. Consumers of `airc-store` interact with these typed shapes;
//! the entity modules (`entities::account_registry`) stay internal.

/// One cached account-registry row. The `document_json` is the
/// serialized `AccountRegistryDocument` from `airc-lib`; this crate
/// stays oblivious to its shape on purpose so the store can carry
/// future document schema changes without recompiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccountRegistry {
    pub mesh_identity: String,
    pub schema_version: u16,
    pub generated_at_ms: u64,
    pub document_json: String,
    pub updated_at_ms: u64,
}

/// One per-mesh-identity gh-gist sentinel row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccountRegistryGistSentinel {
    pub mesh_identity: String,
    pub gist_id: String,
    pub updated_at_ms: u64,
}
