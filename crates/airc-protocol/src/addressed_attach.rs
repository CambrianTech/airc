//! Proof of identity for a daemon's connection-bound addressed inbox attach.
//! The daemon creates a fresh nonce for each socket and consumes the proof on
//! that socket before registering its receiver. This is not a bearer credential.

use airc_core::PeerId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PeerKeyRegistry, PeerKeypair};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachChallenge {
    pub daemon: PeerId,
    pub peer: PeerId,
    pub nonce: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachProof {
    pub key_id: u32,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachAuthError {
    UnknownKey,
    InvalidSignature,
}

impl AttachChallenge {
    fn signed_bytes(&self, key_id: u32) -> Vec<u8> {
        let mut bytes = b"airc/addressed-attach/v1\0".to_vec();
        bytes.extend_from_slice(self.daemon.as_uuid().as_bytes());
        bytes.extend_from_slice(self.peer.as_uuid().as_bytes());
        bytes.extend_from_slice(self.nonce.as_bytes());
        bytes.extend_from_slice(&key_id.to_be_bytes());
        bytes
    }

    pub fn sign(&self, keypair: &PeerKeypair, key_id: u32) -> AttachProof {
        AttachProof {
            key_id,
            signature: keypair.sign_bytes(&self.signed_bytes(key_id)).to_vec(),
        }
    }

    pub fn verify(
        &self,
        proof: &AttachProof,
        registry: &PeerKeyRegistry,
    ) -> Result<(), AttachAuthError> {
        let key = registry
            .lookup(self.peer, proof.key_id)
            .ok_or(AttachAuthError::UnknownKey)?;
        let signature = ed25519_dalek::Signature::from_slice(&proof.signature)
            .map_err(|_| AttachAuthError::InvalidSignature)?;
        key.verify_strict(&self.signed_bytes(proof.key_id), &signature)
            .map_err(|_| AttachAuthError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_binds_identity_daemon_connection_and_key_generation() {
        let registry = PeerKeyRegistry::new();
        let key = PeerKeypair::generate();
        let peer = PeerId::new();
        registry.enrol(peer, 7, key.public_bytes()).unwrap();
        registry.enrol(peer, 8, key.public_bytes()).unwrap();
        let challenge = AttachChallenge {
            daemon: PeerId::new(),
            peer,
            nonce: Uuid::new_v4(),
        };
        let proof = challenge.sign(&key, 7);
        assert_eq!(challenge.verify(&proof, &registry), Ok(()));
        let mut other_connection = challenge.clone();
        other_connection.nonce = Uuid::new_v4();
        assert_eq!(
            other_connection.verify(&proof, &registry),
            Err(AttachAuthError::InvalidSignature)
        );
        let mut other_daemon = challenge.clone();
        other_daemon.daemon = PeerId::new();
        assert_eq!(
            other_daemon.verify(&proof, &registry),
            Err(AttachAuthError::InvalidSignature)
        );
        let mut other_peer = challenge.clone();
        other_peer.peer = PeerId::new();
        registry
            .enrol(other_peer.peer, 7, key.public_bytes())
            .unwrap();
        assert_eq!(
            other_peer.verify(&proof, &registry),
            Err(AttachAuthError::InvalidSignature)
        );
        let mut other_generation = proof.clone();
        other_generation.key_id = 8;
        assert_eq!(
            challenge.verify(&other_generation, &registry),
            Err(AttachAuthError::InvalidSignature)
        );
        let impostor = challenge.sign(&PeerKeypair::generate(), 7);
        assert_eq!(
            challenge.verify(&impostor, &registry),
            Err(AttachAuthError::InvalidSignature)
        );
        registry.remove_peer(peer);
        assert_eq!(
            challenge.verify(&proof, &registry),
            Err(AttachAuthError::UnknownKey)
        );
    }
}
