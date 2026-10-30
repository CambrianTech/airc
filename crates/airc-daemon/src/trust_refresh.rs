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
    UnknownPeer(airc_core::PeerId),
    KeyMismatch(airc_core::PeerId),
    Store(airc_trust::PeersStoreError),
    PubkeyBase64(base64::DecodeError),
    WrongPubkeyLength(usize),
    InvalidPubkey(KeyError),
}

impl std::fmt::Display for TrustRefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPeer(peer) => write!(f, "peer {peer} has no durable trust enrollment"),
            Self::KeyMismatch(peer) => {
                write!(f, "supplied key differs from durable trust for {peer}")
            }
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
            Self::UnknownPeer(_) | Self::KeyMismatch(_) => None,
            TrustRefreshError::Store(error) => Some(error),
            TrustRefreshError::PubkeyBase64(error) => Some(error),
            TrustRefreshError::InvalidPubkey(error) => Some(error),
            TrustRefreshError::WrongPubkeyLength(_) => None,
        }
    }
}

/// Refresh one admission identity only from authoritative durable trust.
/// An IPC caller may name an expected key, but cannot install it.
pub async fn refresh_peer(
    registry: &PeerKeyRegistry,
    root: &Path,
    peer_id: airc_core::PeerId,
    expected: Option<[u8; 32]>,
) -> Result<(), TrustRefreshError> {
    let peer = airc_trust::load(root)
        .await?
        .into_iter()
        .find(|peer| peer.peer_id == peer_id)
        .ok_or(TrustRefreshError::UnknownPeer(peer_id))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(peer.pubkey_b64.as_bytes())
        .map_err(TrustRefreshError::PubkeyBase64)?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| TrustRefreshError::WrongPubkeyLength(bytes.len()))?;
    if expected.is_some_and(|expected| expected != key) {
        return Err(TrustRefreshError::KeyMismatch(peer_id));
    }
    registry
        .enrol(peer_id, 0, key)
        .map_err(TrustRefreshError::InvalidPubkey)
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
