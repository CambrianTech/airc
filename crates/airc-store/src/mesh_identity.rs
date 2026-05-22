//! Mesh identity store types.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMeshIdentity {
    pub scope: String,
    pub identity: String,
    pub source: String,
    pub resolved_at_ms: u64,
    pub ttl_ms: u64,
}
