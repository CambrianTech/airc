//! Backend-neutral DTO for a local identity row.

use airc_core::{identity::Identity, ClientId, PeerId};

pub use crate::entities::local_identity::DEFAULT_AGENT_NAME;

/// Public DTO mirroring a `local_identity` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLocalIdentity {
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub version: u32,
    pub created_at_ms: u64,
    pub identity: Identity,
    /// Discriminator for which agent this row describes (card
    /// 8384cc18 Sub-A). Existing legacy rows carry
    /// [`DEFAULT_AGENT_NAME`]; Sub-D's CLI surface creates distinct
    /// names.
    pub agent_name: String,
    /// Custody binding (card 0f3b3126): the public key this row's secret must
    /// derive at load time. Set by mint and legacy-json migration; `None` on
    /// pre-binding rows, where load has nothing to check against.
    pub expected_public_key: Option<Vec<u8>>,
}
