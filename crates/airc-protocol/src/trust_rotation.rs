//! Signed peer trust rotation — the contract.
//!
//! The audit gap this closes:
//! > "Peer trust rotation is still too permissive: `peers_store::add`
//! > silently replaces a pubkey for the same `PeerId`. That must
//! > become an explicit signed rotation/audit operation."
//!
//! Shape:
//!
//! - A [`TrustRotation`] is the typed envelope. It names the
//!   peer, the previous pubkey, the new pubkey, a monotonic
//!   sequence number, and a wall-clock timestamp (audit-only).
//! - The signature is produced by the previous pubkey's secret
//!   half. Only the holder of the previous key may authorise a
//!   rotation to a new key.
//! - This module verifies the cryptographic shape only:
//!   signature is valid against `prev_pubkey`, and the rotation
//!   isn't a no-op (prev != next).
//! - Anti-replay (sequence > previous-applied) + "prev_pubkey
//!   matches what's currently stored" live in the daemon's
//!   `peers_store::rotate` because they require store state.
//!
//! Out of scope here: cross-machine propagation, multi-sig
//! authorisation, recovery from lost-key.

use ed25519_dalek::{Signature as Ed25519Sig, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::PeerKeypair;

/// Signed authorisation that the holder of `prev_pubkey` is rotating
/// the trust binding for `peer_id` to `next_pubkey`.
///
/// The `signature` covers every preceding field of this struct
/// (everything except `signature` itself) encoded as canonical CBOR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRotation {
    pub peer_id: PeerId,
    /// The pubkey the rotation supersedes. The signature must be
    /// valid against this key.
    pub prev_pubkey: [u8; 32],
    /// The pubkey the rotation authorises. Must differ from
    /// `prev_pubkey` — no-op rotations are rejected.
    pub next_pubkey: [u8; 32],
    /// Per-peer monotonic counter. Strictly increasing across all
    /// rotations applied to this peer. Enforced by the store
    /// (`peers_store::rotate`), not by this module — replay
    /// prevention requires knowing the previous applied sequence.
    pub sequence: u64,
    /// Producer wall-clock at signing time. Audit-only; the daemon
    /// does NOT trust this for security decisions because clock skew
    /// is real and signing machines can lie.
    pub rotated_at_ms: u64,
    /// Ed25519 signature over the canonical CBOR encoding of the
    /// preceding fields, produced by the secret half of `prev_pubkey`.
    #[serde(with = "crate::signature::serde_bytes_64")]
    pub signature: [u8; 64],
}

#[derive(Debug, PartialEq, Eq)]
pub enum RotationVerificationError {
    /// `prev_pubkey` was not a valid Ed25519 verifying key.
    InvalidPrevPubkey,
    /// Signature did not verify against `prev_pubkey`. Most common
    /// failure mode — someone produced a rotation request without
    /// holding the old secret.
    BadSignature,
    /// `prev_pubkey == next_pubkey`. Rotation must change something.
    NoOpRotation,
    /// Canonical CBOR encoding of the rotation body failed.
    CanonicalEncodingFailed,
}

impl std::fmt::Display for RotationVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPrevPubkey => {
                f.write_str("trust rotation: prev_pubkey is not a valid Ed25519 verifying key")
            }
            Self::BadSignature => {
                f.write_str("trust rotation: signature did not verify against prev_pubkey")
            }
            Self::NoOpRotation => {
                f.write_str("trust rotation: prev_pubkey == next_pubkey is a no-op")
            }
            Self::CanonicalEncodingFailed => {
                f.write_str("trust rotation: canonical CBOR encoding of body failed")
            }
        }
    }
}

impl std::error::Error for RotationVerificationError {}

/// Cryptographic verification only. The daemon's `peers_store::rotate`
/// must ALSO check:
/// - `rotation.prev_pubkey` matches what's currently stored for
///   `rotation.peer_id`, and
/// - `rotation.sequence` is strictly greater than the previously
///   applied sequence for this peer.
pub fn verify_rotation(rotation: &TrustRotation) -> Result<(), RotationVerificationError> {
    if rotation.prev_pubkey == rotation.next_pubkey {
        return Err(RotationVerificationError::NoOpRotation);
    }
    let verifying = VerifyingKey::from_bytes(&rotation.prev_pubkey)
        .map_err(|_| RotationVerificationError::InvalidPrevPubkey)?;
    let signed_bytes = canonical_rotation_bytes(rotation)
        .map_err(|_| RotationVerificationError::CanonicalEncodingFailed)?;
    let sig = Ed25519Sig::from_bytes(&rotation.signature);
    verifying
        .verify(&signed_bytes, &sig)
        .map_err(|_| RotationVerificationError::BadSignature)
}

/// Sign a trust rotation. The caller MUST hold the secret half of
/// `prev_keypair`; this method produces the signature over the
/// canonical body so the verifier accepts. Callers that do not
/// hold the previous key cannot produce a passing rotation by
/// construction.
pub fn sign_rotation(
    prev_keypair: &PeerKeypair,
    peer_id: PeerId,
    next_pubkey: [u8; 32],
    sequence: u64,
    rotated_at_ms: u64,
) -> Result<TrustRotation, ciborium::ser::Error<std::io::Error>> {
    let prev_pubkey = prev_keypair.public_bytes();
    let mut rotation = TrustRotation {
        peer_id,
        prev_pubkey,
        next_pubkey,
        sequence,
        rotated_at_ms,
        signature: [0u8; 64],
    };
    let bytes = canonical_rotation_bytes(&rotation)?;
    rotation.signature = prev_keypair.sign_bytes(&bytes);
    Ok(rotation)
}

/// Canonical CBOR bytes of the rotation EXCLUDING the signature.
/// These are the bytes the prev-key signer signs and the verifier
/// verifies against.
pub fn canonical_rotation_bytes(
    rotation: &TrustRotation,
) -> Result<Vec<u8>, ciborium::ser::Error<std::io::Error>> {
    #[derive(Serialize)]
    struct SignedRotationPayload<'r> {
        peer_id: &'r PeerId,
        prev_pubkey: &'r [u8; 32],
        next_pubkey: &'r [u8; 32],
        sequence: u64,
        rotated_at_ms: u64,
    }
    let payload = SignedRotationPayload {
        peer_id: &rotation.peer_id,
        prev_pubkey: &rotation.prev_pubkey,
        next_pubkey: &rotation.next_pubkey,
        sequence: rotation.sequence,
        rotated_at_ms: rotation.rotated_at_ms,
    };
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&payload, &mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_rotation_verifies() {
        let prev = PeerKeypair::generate();
        let next = PeerKeypair::generate();
        let rotation = sign_rotation(
            &prev,
            PeerId::from_u128(0xa1),
            next.public_bytes(),
            1,
            1_700_000_000_000,
        )
        .unwrap();
        assert!(verify_rotation(&rotation).is_ok());
    }

    #[test]
    fn rotation_signed_by_wrong_key_is_rejected() {
        // Build a rotation that CLAIMS prev_pubkey is `prev`'s, but
        // actually sign with `wrong_signer` — a forgery attempt.
        let prev = PeerKeypair::generate();
        let wrong_signer = PeerKeypair::generate();
        let next = PeerKeypair::generate();

        let mut rotation = TrustRotation {
            peer_id: PeerId::from_u128(0xa1),
            prev_pubkey: prev.public_bytes(),
            next_pubkey: next.public_bytes(),
            sequence: 1,
            rotated_at_ms: 1_700_000_000_000,
            signature: [0u8; 64],
        };
        let bytes = canonical_rotation_bytes(&rotation).unwrap();
        rotation.signature = wrong_signer.sign_bytes(&bytes);

        assert_eq!(
            verify_rotation(&rotation),
            Err(RotationVerificationError::BadSignature),
        );
    }

    #[test]
    fn tampered_rotation_after_signing_is_rejected() {
        let prev = PeerKeypair::generate();
        let next = PeerKeypair::generate();
        let attacker_target = PeerKeypair::generate();
        let mut rotation = sign_rotation(
            &prev,
            PeerId::from_u128(0xa1),
            next.public_bytes(),
            1,
            1_700_000_000_000,
        )
        .unwrap();
        // Swap next_pubkey after signing — attacker tries to redirect
        // the rotation to a key they control without re-signing.
        rotation.next_pubkey = attacker_target.public_bytes();
        assert_eq!(
            verify_rotation(&rotation),
            Err(RotationVerificationError::BadSignature),
        );
    }

    #[test]
    fn no_op_rotation_is_rejected() {
        let prev = PeerKeypair::generate();
        let rotation = sign_rotation(
            &prev,
            PeerId::from_u128(0xa1),
            prev.public_bytes(), // same as prev — rotation must change something
            1,
            1_700_000_000_000,
        )
        .unwrap();
        assert_eq!(
            verify_rotation(&rotation),
            Err(RotationVerificationError::NoOpRotation),
        );
    }

    // InvalidPrevPubkey is defensive — dalek 2.x's VerifyingKey decoder
    // accepts arbitrary 32-byte arrays as valid points, so triggering
    // this variant deterministically is tricky. The variant stays for
    // future-proofing (a stricter dalek release would activate it);
    // BadSignature covers the practical rejection path for bogus keys.
}
