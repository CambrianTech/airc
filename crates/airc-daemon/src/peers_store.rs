//! Persisted peer trust registry.
//!
//! Peer trust is durable substrate data. It is backed by
//! `airc-store`/SeaORM tables, not JSON sidecars.

use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use airc_core::PeerId;
use airc_protocol::trust_rotation::{verify_rotation, RotationVerificationError, TrustRotation};
pub use airc_store::{RotationAuditEntry, StoredPeer};
use airc_store::{SqliteEventStore, StoreError};

const STORE_DB_FILENAME: &str = "events.sqlite";

#[derive(Debug)]
pub enum PeersStoreError {
    Io(std::io::Error),
    Base64(base64::DecodeError),
    Clock(std::time::SystemTimeError),
    Store(StoreError),
    WrongPubkeyLength(usize),
    /// Trust gap §8 fix: a peer with this `PeerId` is already in the
    /// store with a different pubkey. To change a stored pubkey, the
    /// caller must use [`rotate`] with a [`TrustRotation`] signed by
    /// the currently-stored key. Silent overwrite is forbidden.
    PubkeyConflict {
        peer_id: PeerId,
        stored_pubkey_b64: String,
        attempted_pubkey_b64: String,
    },
    /// Crypto-level verification of the rotation failed.
    RotationVerification(RotationVerificationError),
    /// Rotation supplied a `prev_pubkey` that doesn't match what's
    /// currently stored for the target `peer_id`. Either the rotation
    /// is stale (already superseded) or signed against a different
    /// trust state than this store has.
    PrevPubkeyMismatch {
        peer_id: PeerId,
        stored_pubkey_b64: String,
        rotation_prev_pubkey_b64: String,
    },
    /// Rotation sequence number is not strictly greater than the
    /// previously-applied rotation. Either replay or out-of-order.
    SequenceNotMonotonic {
        peer_id: PeerId,
        last_applied: u64,
        attempted: u64,
    },
    /// Trying to rotate a peer that isn't enrolled. The caller must
    /// `add` first (trust-on-first-use), then `rotate` from then on.
    UnknownPeer(PeerId),
}

impl std::fmt::Display for PeersStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeersStoreError::Io(error) => write!(f, "peer trust I/O: {error}"),
            PeersStoreError::Base64(error) => write!(f, "peer trust base64: {error}"),
            PeersStoreError::Clock(error) => write!(f, "peer trust timestamp clock error: {error}"),
            PeersStoreError::Store(error) => write!(f, "peer trust store: {error}"),
            PeersStoreError::WrongPubkeyLength(got) => {
                write!(f, "peer trust pubkey is {got} bytes, expected 32")
            }
            PeersStoreError::PubkeyConflict {
                peer_id,
                stored_pubkey_b64,
                attempted_pubkey_b64,
            } => write!(
                f,
                "peer {peer_id} is already enrolled with pubkey {stored_pubkey_b64}; \
                 cannot silently overwrite with {attempted_pubkey_b64}. Use `rotate` \
                 with a TrustRotation signed by the currently-stored key."
            ),
            PeersStoreError::RotationVerification(error) => {
                write!(f, "trust rotation rejected: {error}")
            }
            PeersStoreError::PrevPubkeyMismatch {
                peer_id,
                stored_pubkey_b64,
                rotation_prev_pubkey_b64,
            } => write!(
                f,
                "trust rotation for {peer_id} names prev_pubkey {rotation_prev_pubkey_b64} \
                 but stored pubkey is {stored_pubkey_b64}; rotation is stale or for a \
                 different trust state."
            ),
            PeersStoreError::SequenceNotMonotonic {
                peer_id,
                last_applied,
                attempted,
            } => write!(
                f,
                "trust rotation for {peer_id} has sequence {attempted}, not strictly greater \
                 than last-applied {last_applied}; possible replay."
            ),
            PeersStoreError::UnknownPeer(peer_id) => {
                write!(
                    f,
                    "trust rotation references unknown peer {peer_id}; enrol via add() first"
                )
            }
        }
    }
}

impl std::error::Error for PeersStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PeersStoreError::Io(error) => Some(error),
            PeersStoreError::Base64(error) => Some(error),
            PeersStoreError::Clock(error) => Some(error),
            PeersStoreError::Store(error) => Some(error),
            PeersStoreError::RotationVerification(error) => Some(error),
            PeersStoreError::WrongPubkeyLength(_)
            | PeersStoreError::PubkeyConflict { .. }
            | PeersStoreError::PrevPubkeyMismatch { .. }
            | PeersStoreError::SequenceNotMonotonic { .. }
            | PeersStoreError::UnknownPeer(_) => None,
        }
    }
}

impl From<RotationVerificationError> for PeersStoreError {
    fn from(error: RotationVerificationError) -> Self {
        PeersStoreError::RotationVerification(error)
    }
}

impl From<std::io::Error> for PeersStoreError {
    fn from(error: std::io::Error) -> Self {
        PeersStoreError::Io(error)
    }
}

impl From<base64::DecodeError> for PeersStoreError {
    fn from(error: base64::DecodeError) -> Self {
        PeersStoreError::Base64(error)
    }
}

impl From<std::time::SystemTimeError> for PeersStoreError {
    fn from(error: std::time::SystemTimeError) -> Self {
        PeersStoreError::Clock(error)
    }
}

impl From<StoreError> for PeersStoreError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::PeerPubkeyConflict {
                peer_id,
                stored_pubkey_b64,
                attempted_pubkey_b64,
            } => PeersStoreError::PubkeyConflict {
                peer_id,
                stored_pubkey_b64,
                attempted_pubkey_b64,
            },
            StoreError::WrongPubkeyLength(got) => PeersStoreError::WrongPubkeyLength(got),
            StoreError::Base64(error) => PeersStoreError::Base64(error),
            other => PeersStoreError::Store(other),
        }
    }
}

fn store_path(home: &Path) -> std::path::PathBuf {
    home.join(STORE_DB_FILENAME)
}

async fn open_store(home: &Path) -> Result<SqliteEventStore, PeersStoreError> {
    Ok(SqliteEventStore::open_path(&store_path(home)).await?)
}

/// Load the peer list from `home`. Returns an empty list when no
/// peer trust rows exist yet (normal for a fresh install).
pub async fn load(home: &Path) -> Result<Vec<StoredPeer>, PeersStoreError> {
    open_store(home)
        .await?
        .load_peers()
        .await
        .map_err(Into::into)
}

/// Enrol a peer's pubkey for the first time (trust-on-first-use).
///
/// Behaviour:
/// - Peer not in store: append, persist, return the new entry.
/// - Peer in store with the SAME pubkey: idempotent no-op, return
///   the stored entry.
/// - Peer in store with a DIFFERENT pubkey: refuses with
///   [`PeersStoreError::PubkeyConflict`]. Changing a stored pubkey
///   requires an explicit signed [`rotate`] — silent overwrite was
///   the trust-store gap §8 fix removed.
pub async fn add(
    home: &Path,
    peer_id: PeerId,
    pubkey: [u8; 32],
) -> Result<StoredPeer, PeersStoreError> {
    let pubkey_b64 = URL_SAFE_NO_PAD.encode(pubkey);
    open_store(home)
        .await?
        .add_peer_trust(peer_id, pubkey_b64, now_ms()?)
        .await
        .map_err(Into::into)
}

/// Apply a signed trust rotation:
///
/// 1. Verify cryptographic shape (signature against `prev_pubkey`,
///    not a no-op) via [`airc_protocol::trust_rotation::verify_rotation`].
/// 2. Check the rotation's `prev_pubkey` matches the currently-stored
///    pubkey for `rotation.peer_id`. A stale rotation against a
///    superseded key is rejected.
/// 3. Check the rotation's `sequence` is strictly greater than the
///    last applied sequence for this peer (replay prevention).
/// 4. Insert an audit row into `peer_rotation_audit`.
/// 5. Persist the new pubkey to `peer_trust`.
pub async fn rotate(home: &Path, rotation: &TrustRotation) -> Result<StoredPeer, PeersStoreError> {
    // Step 1: crypto.
    verify_rotation(rotation)?;

    // Step 2: stored prev_pubkey matches what rotation names.
    let store = open_store(home).await?;
    let peers = store.load_peers().await?;
    let stored_idx = peers
        .iter()
        .position(|p| p.peer_id == rotation.peer_id)
        .ok_or(PeersStoreError::UnknownPeer(rotation.peer_id))?;
    let stored_pubkey_b64 = peers[stored_idx].pubkey_b64.clone();
    let rotation_prev_pubkey_b64 = URL_SAFE_NO_PAD.encode(rotation.prev_pubkey);
    if stored_pubkey_b64 != rotation_prev_pubkey_b64 {
        return Err(PeersStoreError::PrevPubkeyMismatch {
            peer_id: rotation.peer_id,
            stored_pubkey_b64,
            rotation_prev_pubkey_b64,
        });
    }

    // Step 3: monotonic sequence per peer.
    let last_seq = last_applied_sequence(&store, rotation.peer_id).await?;
    if rotation.sequence <= last_seq {
        return Err(PeersStoreError::SequenceNotMonotonic {
            peer_id: rotation.peer_id,
            last_applied: last_seq,
            attempted: rotation.sequence,
        });
    }

    // Step 4: append audit entry.
    let applied_at_ms = now_ms()?;
    let audit_entry = RotationAuditEntry {
        peer_id: rotation.peer_id,
        prev_pubkey_b64: rotation_prev_pubkey_b64,
        next_pubkey_b64: URL_SAFE_NO_PAD.encode(rotation.next_pubkey),
        sequence: rotation.sequence,
        rotated_at_ms: rotation.rotated_at_ms,
        applied_at_ms,
    };
    store.append_peer_rotation_audit(audit_entry).await?;

    // Step 5: replace peer_trust with the new pubkey.
    store
        .replace_peer_trust(
            rotation.peer_id,
            URL_SAFE_NO_PAD.encode(rotation.next_pubkey),
            applied_at_ms,
        )
        .await
        .map_err(Into::into)
}

/// Read all audit entries for `peer_id` in apply-order. Returns an
/// empty vec if the audit log doesn't exist yet (no rotations have
/// ever been applied) or the peer has none.
pub async fn audit_log(
    home: &Path,
    peer_id: PeerId,
) -> Result<Vec<RotationAuditEntry>, PeersStoreError> {
    open_store(home)
        .await?
        .peer_rotation_audit(peer_id)
        .await
        .map_err(Into::into)
}

async fn last_applied_sequence(
    store: &SqliteEventStore,
    peer_id: PeerId,
) -> Result<u64, PeersStoreError> {
    Ok(store
        .peer_rotation_audit(peer_id)
        .await?
        .into_iter()
        .map(|e| e.sequence)
        .max()
        .unwrap_or(0))
}

fn now_ms() -> Result<u64, std::time::SystemTimeError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_pubkey(seed: u8) -> [u8; 32] {
        let mut k = [0u8; 32];
        k[0] = seed;
        // Avoid the all-zero pubkey which Ed25519 rejects. We're not
        // actually verifying these in peers_store tests, but the
        // shape matters.
        k[31] = seed.wrapping_add(1);
        k
    }

    #[tokio::test]
    async fn load_returns_empty_when_store_has_no_peer_rows() {
        let home = TempDir::new().unwrap();
        let peers = load(home.path()).await.unwrap();
        assert!(peers.is_empty());
    }

    #[tokio::test]
    async fn add_then_load_roundtrips() {
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let pk = fake_pubkey(0xaa);
        let stored = add(home.path(), id, pk).await.unwrap();
        assert_eq!(stored.peer_id, id);
        assert_eq!(stored.pubkey_bytes().unwrap(), pk);

        let loaded = load(home.path()).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].peer_id, id);
        assert_eq!(loaded[0].pubkey_bytes().unwrap(), pk);
    }

    #[tokio::test]
    async fn add_is_idempotent_for_same_pubkey() {
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let pk = fake_pubkey(0xbb);
        let first = add(home.path(), id, pk).await.unwrap();
        let second = add(home.path(), id, pk).await.unwrap();
        // Same added_at_ms because second call returns the already-
        // stored entry (didn't overwrite).
        assert_eq!(first.added_at_ms, second.added_at_ms);
        let loaded = load(home.path()).await.unwrap();
        assert_eq!(loaded.len(), 1, "duplicate enrolment must be deduped");
    }

    #[tokio::test]
    async fn add_refuses_silent_overwrite_on_pubkey_conflict() {
        // Grievance §8 fix: silent overwrite on second `add` of the
        // same peer_id with a new pubkey is what the rotation path
        // replaces. `add` must now refuse and surface a typed error.
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let old = fake_pubkey(0xc0);
        let new = fake_pubkey(0xc1);
        add(home.path(), id, old).await.unwrap();
        let result = add(home.path(), id, new).await;
        assert!(
            matches!(result, Err(PeersStoreError::PubkeyConflict { peer_id, .. }) if peer_id == id),
            "second add() with a different pubkey must error PubkeyConflict; got {result:?}",
        );
        // Old pubkey must still be stored — no partial-overwrite.
        let loaded = load(home.path()).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].pubkey_bytes().unwrap(), old);
    }

    #[tokio::test]
    async fn add_is_idempotent_for_same_peer_same_pubkey() {
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let key = fake_pubkey(0x42);
        let first = add(home.path(), id, key).await.unwrap();
        let second = add(home.path(), id, key).await.unwrap();
        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(first.pubkey_b64, second.pubkey_b64);
        let loaded = load(home.path()).await.unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[tokio::test]
    async fn signed_rotation_is_accepted_and_audited() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let prev_kp = PeerKeypair::generate();
        let next_kp = PeerKeypair::generate();

        add(home.path(), peer_id, prev_kp.public_bytes())
            .await
            .unwrap();

        let rotation = sign_rotation(
            &prev_kp,
            peer_id,
            next_kp.public_bytes(),
            1,
            1_700_000_000_000,
        )
        .unwrap();

        let updated = rotate(home.path(), &rotation).await.unwrap();
        assert_eq!(updated.pubkey_bytes().unwrap(), next_kp.public_bytes());

        // Audit trail visible to consumers.
        let audit = audit_log(home.path(), peer_id).await.unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].sequence, 1);
        assert_eq!(audit[0].rotated_at_ms, 1_700_000_000_000);
    }

    #[tokio::test]
    async fn rotation_signed_by_wrong_key_is_rejected_before_audit() {
        use airc_protocol::{sign_rotation, PeerKeypair, RotationVerificationError};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let prev_kp = PeerKeypair::generate();
        let next_kp = PeerKeypair::generate();
        let imposter_kp = PeerKeypair::generate();

        add(home.path(), peer_id, prev_kp.public_bytes())
            .await
            .unwrap();

        // Imposter forges a rotation pretending to be prev, signed by
        // their own key. Crypto verification fails before we touch the
        // store.
        let bad = sign_rotation(
            &imposter_kp,
            peer_id,
            next_kp.public_bytes(),
            1,
            1_700_000_000_000,
        )
        .map(|mut r| {
            // The rotation as signed names imposter as prev_pubkey.
            // To make the test exercise BadSignature against prev,
            // forge prev_pubkey post-signing.
            r.prev_pubkey = prev_kp.public_bytes();
            r
        })
        .unwrap();

        let result = rotate(home.path(), &bad).await;
        assert!(
            matches!(
                result,
                Err(PeersStoreError::RotationVerification(
                    RotationVerificationError::BadSignature
                ))
            ),
            "wrong-key rotation must fail crypto check before touching store; got {result:?}",
        );
        // Store untouched.
        let loaded = load(home.path()).await.unwrap();
        assert_eq!(loaded[0].pubkey_bytes().unwrap(), prev_kp.public_bytes());
        assert!(audit_log(home.path(), peer_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_rotation_against_already_superseded_key_is_rejected() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let k0 = PeerKeypair::generate();
        let k1 = PeerKeypair::generate();
        let k2 = PeerKeypair::generate();

        add(home.path(), peer_id, k0.public_bytes()).await.unwrap();
        let r1 = sign_rotation(&k0, peer_id, k1.public_bytes(), 1, 1).unwrap();
        rotate(home.path(), &r1).await.unwrap();

        // Now the stored key is k1. Replay r1 (or any rotation whose
        // prev_pubkey is k0) must be rejected — k0 was superseded.
        let replay = sign_rotation(&k0, peer_id, k2.public_bytes(), 2, 2).unwrap();
        let result = rotate(home.path(), &replay).await;
        assert!(
            matches!(
                result,
                Err(PeersStoreError::PrevPubkeyMismatch { peer_id: p, .. }) if p == peer_id
            ),
            "rotation against a superseded prev_pubkey must error PrevPubkeyMismatch; got {result:?}",
        );
    }

    #[tokio::test]
    async fn rotation_with_non_monotonic_sequence_is_rejected() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let k0 = PeerKeypair::generate();
        let k1 = PeerKeypair::generate();
        let k2 = PeerKeypair::generate();

        add(home.path(), peer_id, k0.public_bytes()).await.unwrap();
        let r1 = sign_rotation(&k0, peer_id, k1.public_bytes(), 5, 100).unwrap();
        rotate(home.path(), &r1).await.unwrap();

        // Try to apply a rotation with sequence == previous (replay) and
        // sequence < previous (out-of-order). Both must fail.
        let same_seq = sign_rotation(&k1, peer_id, k2.public_bytes(), 5, 200).unwrap();
        let lower_seq = sign_rotation(&k1, peer_id, k2.public_bytes(), 3, 200).unwrap();
        assert!(matches!(
            rotate(home.path(), &same_seq).await,
            Err(PeersStoreError::SequenceNotMonotonic {
                last_applied: 5,
                attempted: 5,
                ..
            })
        ));
        assert!(matches!(
            rotate(home.path(), &lower_seq).await,
            Err(PeersStoreError::SequenceNotMonotonic {
                last_applied: 5,
                attempted: 3,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn rotation_for_unknown_peer_is_rejected() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let prev_kp = PeerKeypair::generate();
        let next_kp = PeerKeypair::generate();

        // Note: NO add() — peer isn't enrolled.
        let rotation = sign_rotation(&prev_kp, peer_id, next_kp.public_bytes(), 1, 0).unwrap();
        let result = rotate(home.path(), &rotation).await;
        assert!(matches!(
            result,
            Err(PeersStoreError::UnknownPeer(p)) if p == peer_id
        ));
    }

    #[tokio::test]
    async fn multiple_distinct_peers_accumulate() {
        let home = TempDir::new().unwrap();
        let a = (PeerId::new(), fake_pubkey(0x01));
        let b = (PeerId::new(), fake_pubkey(0x02));
        let c = (PeerId::new(), fake_pubkey(0x03));
        add(home.path(), a.0, a.1).await.unwrap();
        add(home.path(), b.0, b.1).await.unwrap();
        add(home.path(), c.0, c.1).await.unwrap();
        let loaded = load(home.path()).await.unwrap();
        assert_eq!(loaded.len(), 3);
        let ids: Vec<PeerId> = loaded.iter().map(|p| p.peer_id).collect();
        assert!(ids.contains(&a.0));
        assert!(ids.contains(&b.0));
        assert!(ids.contains(&c.0));
    }
}
