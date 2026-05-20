//! Parse `--peer <id>:<pubkey-b64>` arguments into a `PeerKeyRegistry`.
//!
//! MVP pairing: peers exchange their (PeerId, pubkey) pair out-of-band
//! and feed them to the CLI as repeated `--peer` flags. A real airc
//! deployment will have a pairing flow (QR codes, invite URLs, signed
//! enrolments via a coordinator). This module is the minimum that
//! makes cross-process demos work.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::RwLock;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::PeerId;
use airc_protocol::PeerKeyRegistry;

#[derive(Debug)]
pub enum PeerSpecError {
    /// The argument didn't have the form `<peer_id>:<base64_pubkey>`.
    BadFormat(String),
    /// The `peer_id` portion wasn't a valid UUID.
    BadPeerId { input: String, source: uuid::Error },
    /// Base64 decode failed.
    BadBase64(String),
    /// Decoded pubkey wasn't 32 bytes.
    WrongPubkeyLength(usize),
}

impl std::fmt::Display for PeerSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerSpecError::BadFormat(input) => write!(
                f,
                "peer spec {input:?} is not in the form <uuid>:<base64-pubkey>"
            ),
            PeerSpecError::BadPeerId { input, source } => {
                write!(f, "peer id {input:?} is not a valid UUID: {source}")
            }
            PeerSpecError::BadBase64(message) => write!(f, "base64 decode failed: {message}"),
            PeerSpecError::WrongPubkeyLength(got) => {
                write!(f, "decoded pubkey is {got} bytes, expected 32")
            }
        }
    }
}

impl std::error::Error for PeerSpecError {}

/// A parsed peer spec: which peer it identifies + their 32-byte pubkey.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerSpec {
    pub peer_id: PeerId,
    pub pubkey: [u8; 32],
}

impl FromStr for PeerSpec {
    type Err = PeerSpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (id_part, key_part) = s
            .split_once(':')
            .ok_or_else(|| PeerSpecError::BadFormat(s.to_string()))?;
        let uuid = Uuid::parse_str(id_part).map_err(|source| PeerSpecError::BadPeerId {
            input: id_part.to_string(),
            source,
        })?;
        let bytes = URL_SAFE_NO_PAD
            .decode(key_part)
            .map_err(|error| PeerSpecError::BadBase64(error.to_string()))?;
        if bytes.len() != 32 {
            return Err(PeerSpecError::WrongPubkeyLength(bytes.len()));
        }
        let mut pubkey = [0u8; 32];
        pubkey.copy_from_slice(&bytes);
        Ok(PeerSpec {
            peer_id: PeerId::from_uuid(uuid),
            pubkey,
        })
    }
}

/// Build a registry from a list of peer specs. Each spec's pubkey is
/// enrolled at key_id = 0 (the substrate default).
// Kept available for tests; the CLI command path now uses
// `commands::build_combined_registry` which unions persistent +
// ad-hoc peers under one helper.
#[allow(dead_code)]
pub fn build_registry(
    self_peer_id: PeerId,
    self_pubkey: [u8; 32],
    peer_specs: &[PeerSpec],
) -> Result<Arc<RwLock<PeerKeyRegistry>>, airc_protocol::KeyError> {
    let mut registry = PeerKeyRegistry::new();
    registry.enrol(self_peer_id, 0, self_pubkey)?;
    for spec in peer_specs {
        registry.enrol(spec.peer_id, 0, spec.pubkey)?;
    }
    Ok(Arc::new(RwLock::new(registry)))
}

/// Encode a pubkey as a `--peer` argument tail (`<id>:<b64>`), for
/// printing on `init` so the user can hand it to peers out-of-band.
pub fn format_peer_spec(peer_id: PeerId, pubkey: &[u8; 32]) -> String {
    let encoded = URL_SAFE_NO_PAD.encode(pubkey);
    format!("{peer_id}:{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_protocol::PeerKeypair;

    #[test]
    fn round_trip_format_then_parse() {
        let peer_id = PeerId::new();
        let keypair = PeerKeypair::generate();
        let spec_str = format_peer_spec(peer_id, &keypair.public_bytes());
        let parsed: PeerSpec = spec_str.parse().unwrap();
        assert_eq!(parsed.peer_id, peer_id);
        assert_eq!(parsed.pubkey, keypair.public_bytes());
    }

    #[test]
    fn rejects_missing_colon() {
        let result: Result<PeerSpec, _> = "no-colon-here".parse();
        assert!(matches!(result, Err(PeerSpecError::BadFormat(_))));
    }

    #[test]
    fn rejects_bad_uuid() {
        let result: Result<PeerSpec, _> = "not-a-uuid:AAAA".parse();
        assert!(matches!(result, Err(PeerSpecError::BadPeerId { .. })));
    }

    #[test]
    fn rejects_short_pubkey() {
        let uuid = PeerId::new();
        let short_b64 = URL_SAFE_NO_PAD.encode([0u8; 10]);
        let spec_str = format!("{uuid}:{short_b64}");
        let result: Result<PeerSpec, _> = spec_str.parse();
        assert!(matches!(result, Err(PeerSpecError::WrongPubkeyLength(10))));
    }

    #[test]
    fn build_registry_with_two_peers() {
        let self_id = PeerId::new();
        let self_kp = PeerKeypair::generate();
        let other_id = PeerId::new();
        let other_kp = PeerKeypair::generate();
        let spec = PeerSpec {
            peer_id: other_id,
            pubkey: other_kp.public_bytes(),
        };
        let registry = build_registry(self_id, self_kp.public_bytes(), &[spec]).unwrap();
        let registry = registry.read().unwrap();
        assert!(registry.lookup(self_id, 0).is_some());
        assert!(registry.lookup(other_id, 0).is_some());
    }
}
