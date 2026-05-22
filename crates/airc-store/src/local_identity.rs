//! Backend-neutral DTO for the singleton local identity row.

use airc_core::{identity::Identity, ClientId, PeerId};

/// Public DTO mirroring the singleton `local_identity` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLocalIdentity {
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub version: u32,
    pub created_at_ms: u64,
    pub identity: Identity,
}
