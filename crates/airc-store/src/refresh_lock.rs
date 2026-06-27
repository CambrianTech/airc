//! Store-backed singleflight lock for rare remote registry refreshes.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRefreshLock {
    pub mesh_identity: String,
    pub held_at_ms: u64,
    pub holder_pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredRefreshLockOutcome {
    Acquired,
    HeldFresh { held_at_ms: u64 },
}
