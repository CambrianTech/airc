//! Peer trust store types.

use airc_core::PeerId;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::StoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPeer {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
    pub added_at_ms: u64,
}

impl StoredPeer {
    pub fn pubkey_bytes(&self) -> Result<[u8; 32], StoreError> {
        let bytes = URL_SAFE_NO_PAD.decode(&self.pubkey_b64)?;
        if bytes.len() != 32 {
            return Err(StoreError::WrongPubkeyLength(bytes.len()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationAuditEntry {
    pub peer_id: PeerId,
    pub prev_pubkey_b64: String,
    pub next_pubkey_b64: String,
    pub sequence: u64,
    pub rotated_at_ms: u64,
    pub applied_at_ms: u64,
}
