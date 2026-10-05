//! Sealing for a citizen's private space ("her mind store"), continuum's
//! `docs/architecture/PRIVACY-OF-THOUGHT.md` §3–§4.
//!
//! Two keys, deliberately separate:
//! - the **seal key** is derived from her identity secret (HKDF-SHA256, domain-separated,
//!   bound to her peer id). Only the holder of her identity secret can derive it.
//! - the **mind key** (`K_mind`) is random and encrypts her records. It is stored sealed
//!   under the seal key, so rotating her identity re-seals one small key and never
//!   re-encrypts the records.
//!
//! Every sealed blob is `nonce (12) || ciphertext+tag`, ChaCha20-Poly1305 with a fresh
//! random nonce. The associated data binds a blob to where it belongs (her peer id, the
//! record id), so a blob cannot be replayed into another mind or another record.
//!
//! Pure functions, no I/O: the store that uses them lives in `airc-identity::mind`.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;

/// Domain separation for the seal key. Fixed forever: changing it would orphan every
/// sealed mind key.
const SEAL_KEY_SALT: &[u8] = b"airc-mind-seal-v1";
const NONCE_LEN: usize = 12;

/// A 32-byte symmetric key.
pub type SymmetricKey = [u8; 32];

#[derive(Debug, PartialEq, Eq)]
pub enum SealError {
    /// Shorter than a nonce plus a tag: not a sealed blob.
    Truncated(usize),
    /// Authentication failed: the wrong key, the wrong associated data, or tampering.
    /// Deliberately one variant; the three are indistinguishable by design.
    Unauthentic,
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SealError::Truncated(n) => write!(
                f,
                "sealed blob is {n} bytes, shorter than a nonce and a tag"
            ),
            SealError::Unauthentic => {
                write!(f, "sealed blob does not open under this key and binding")
            }
        }
    }
}

impl std::error::Error for SealError {}

/// Her seal key: derived from her identity secret and bound to her peer id, so two
/// identities never share one, even from a reused secret.
pub fn mind_seal_key(identity_secret: &[u8; 32], peer_id: &[u8]) -> SymmetricKey {
    let hk = Hkdf::<Sha256>::new(Some(SEAL_KEY_SALT), identity_secret);
    let mut key = [0u8; 32];
    hk.expand(peer_id, &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// A fresh random key (her mind key, made once).
pub fn random_key() -> SymmetricKey {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// Seal `plaintext` under `key`, bound to `associated`. Returns `nonce || ciphertext+tag`.
pub fn seal(key: &SymmetricKey, plaintext: &[u8], associated: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: associated,
            },
        )
        .expect("ChaCha20-Poly1305 encryption of an in-memory buffer cannot fail");
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

/// Open a blob made by [`seal`] with the same key and associated data.
pub fn open(key: &SymmetricKey, sealed: &[u8], associated: &[u8]) -> Result<Vec<u8>, SealError> {
    const TAG_LEN: usize = 16;
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(SealError::Truncated(sealed.len()));
    }
    let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: associated,
            },
        )
        .map_err(|_| SealError::Unauthentic)
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a seal that opens under the wrong key or the wrong binding (one
    // mind reading another's record, or a record replayed under another id), a seal key
    // that two identities share, or a sealed blob that leaks its plaintext.
    #[test]
    fn a_sealed_record_opens_only_for_her_key_and_its_own_binding() {
        let secret = [7u8; 32];
        let her = mind_seal_key(&secret, b"peer-her");
        assert_ne!(
            her,
            mind_seal_key(&secret, b"peer-other"),
            "bound to her peer id"
        );
        assert_ne!(
            her,
            mind_seal_key(&[8u8; 32], b"peer-her"),
            "bound to her secret"
        );
        assert_eq!(her, mind_seal_key(&secret, b"peer-her"), "stable");

        let sealed = seal(&her, b"a private plan", b"record-1");
        assert!(
            !sealed.windows(14).any(|w| w == b"a private plan"),
            "no plaintext in the blob"
        );
        assert_eq!(open(&her, &sealed, b"record-1").unwrap(), b"a private plan");
        assert_eq!(
            open(&her, &sealed, b"record-2"),
            Err(SealError::Unauthentic),
            "bound to its record"
        );
        assert_eq!(
            open(&random_key(), &sealed, b"record-1"),
            Err(SealError::Unauthentic),
            "her key only"
        );
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            open(&her, &tampered, b"record-1"),
            Err(SealError::Unauthentic)
        );
        assert_eq!(
            open(&her, &sealed[..20], b"record-1"),
            Err(SealError::Truncated(20))
        );
        assert_ne!(
            seal(&her, b"x", b"r"),
            seal(&her, b"x", b"r"),
            "a fresh nonce every seal"
        );
    }
}
