//! Synchronize durable peer trust into the daemon's live verifier.
//!
//! The ORM-backed trust store is the source of truth. The daemon's
//! `PeerKeyRegistry` is the hot-path verifier cache used by signed
//! transports. This module is the bridge between those layers: small,
//! explicit, and one-way.

use std::path::Path;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use airc_protocol::{KeyError, PeerKeyRegistry};

#[derive(Debug)]
pub enum TrustRefreshError {
    Store(airc_trust::PeersStoreError),
    PubkeyBase64(base64::DecodeError),
    WrongPubkeyLength(usize),
    InvalidPubkey(KeyError),
}

impl std::fmt::Display for TrustRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustRefreshError::Store(error) => write!(f, "trust refresh store: {error}"),
            TrustRefreshError::PubkeyBase64(error) => {
                write!(f, "trust refresh pubkey base64: {error}")
            }
            TrustRefreshError::WrongPubkeyLength(got) => {
                write!(f, "trust refresh pubkey is {got} bytes, expected 32")
            }
            TrustRefreshError::InvalidPubkey(error) => {
                write!(f, "trust refresh invalid pubkey: {error}")
            }
        }
    }
}

impl std::error::Error for TrustRefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TrustRefreshError::Store(error) => Some(error),
            TrustRefreshError::PubkeyBase64(error) => Some(error),
            TrustRefreshError::InvalidPubkey(error) => Some(error),
            TrustRefreshError::WrongPubkeyLength(_) => None,
        }
    }
}

impl From<airc_trust::PeersStoreError> for TrustRefreshError {
    fn from(error: airc_trust::PeersStoreError) -> Self {
        TrustRefreshError::Store(error)
    }
}

pub async fn refresh_root(
    registry: Arc<PeerKeyRegistry>,
    root: &Path,
) -> Result<usize, TrustRefreshError> {
    let peers = airc_trust::load(root).await?;
    let mut enrolled = 0;
    for peer in peers {
        let bytes = URL_SAFE_NO_PAD
            .decode(peer.pubkey_b64.as_bytes())
            .map_err(TrustRefreshError::PubkeyBase64)?;
        let pubkey: [u8; 32] = bytes
            .try_into()
            .map_err(|bytes: Vec<u8>| TrustRefreshError::WrongPubkeyLength(bytes.len()))?;
        registry
            .enrol(peer.peer_id, 0, pubkey)
            .map_err(TrustRefreshError::InvalidPubkey)?;
        enrolled += 1;
    }
    Ok(enrolled)
}
