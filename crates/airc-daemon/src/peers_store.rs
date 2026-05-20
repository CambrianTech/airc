//! Persisted peer registry — `<home>/peers.json`.
//!
//! Saves enrolled peers across CLI / daemon restarts so `--peer
//! <spec>` flags disappear from daily use. Two writers:
//!   - `airc-rs peer add <spec>` — appends to the file
//!   - The daemon's `AddPeer` handler — appends + reloads its
//!     in-memory `PeerKeyRegistry`
//!
//! Schema is versioned (`version: 1`) so future shape changes don't
//! silently misread older files.
//!
//! Storage caveat: pubkeys are not secret, but the registry IS the
//! trust anchor — anyone who can write this file can enrol an
//! impostor. Permissions match the identity files (0600 on Unix).

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use airc_core::PeerId;
use airc_protocol::trust_rotation::{verify_rotation, RotationVerificationError, TrustRotation};

const PEERS_FILENAME: &str = "peers.json";
const PEERS_AUDIT_FILENAME: &str = "peers_audit.jsonl";
const PEERS_VERSION: u32 = 1;

#[derive(Debug)]
pub enum PeersStoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    Clock(std::time::SystemTimeError),
    SchemaVersionMismatch {
        found: u32,
        expected: u32,
    },
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
            PeersStoreError::Io(error) => write!(f, "peers.json I/O: {error}"),
            PeersStoreError::Json(error) => write!(f, "peers.json parse: {error}"),
            PeersStoreError::Base64(error) => write!(f, "peers.json base64: {error}"),
            PeersStoreError::Clock(error) => write!(f, "peers.json timestamp clock error: {error}"),
            PeersStoreError::SchemaVersionMismatch { found, expected } => {
                write!(f, "peers.json version {found}, expected {expected}")
            }
            PeersStoreError::WrongPubkeyLength(got) => {
                write!(f, "peers.json pubkey is {got} bytes, expected 32")
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
            PeersStoreError::Json(error) => Some(error),
            PeersStoreError::Base64(error) => Some(error),
            PeersStoreError::Clock(error) => Some(error),
            PeersStoreError::RotationVerification(error) => Some(error),
            PeersStoreError::SchemaVersionMismatch { .. }
            | PeersStoreError::WrongPubkeyLength(_)
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

impl From<serde_json::Error> for PeersStoreError {
    fn from(error: serde_json::Error) -> Self {
        PeersStoreError::Json(error)
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

/// One persisted peer entry — what the file holds. `pubkey_b64` is
/// the URL-safe-no-padding encoding of the 32-byte Ed25519 pubkey
/// (matches the `peer add <spec>` argument shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredPeer {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
    pub added_at_ms: u64,
}

impl StoredPeer {
    /// Decode the stored base64 pubkey to its 32-byte form. Used when
    /// enroling into a `PeerKeyRegistry`.
    pub fn pubkey_bytes(&self) -> Result<[u8; 32], PeersStoreError> {
        let bytes = URL_SAFE_NO_PAD.decode(&self.pubkey_b64)?;
        if bytes.len() != 32 {
            return Err(PeersStoreError::WrongPubkeyLength(bytes.len()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PeersFile {
    version: u32,
    peers: Vec<StoredPeer>,
}

/// Path to peers.json inside `home`.
pub fn path_in(home: &Path) -> PathBuf {
    home.join(PEERS_FILENAME)
}

/// Load the peer list from `home`. Returns an empty list if the file
/// doesn't exist (this is the normal state for a fresh install).
pub fn load(home: &Path) -> Result<Vec<StoredPeer>, PeersStoreError> {
    let path = path_in(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let file: PeersFile = serde_json::from_str(&text)?;
    if file.version != PEERS_VERSION {
        return Err(PeersStoreError::SchemaVersionMismatch {
            found: file.version,
            expected: PEERS_VERSION,
        });
    }
    Ok(file.peers)
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
pub fn add(home: &Path, peer_id: PeerId, pubkey: [u8; 32]) -> Result<StoredPeer, PeersStoreError> {
    let mut peers = load(home)?;
    let pubkey_b64 = URL_SAFE_NO_PAD.encode(pubkey);

    if let Some(existing) = peers.iter().find(|p| p.peer_id == peer_id) {
        if existing.pubkey_b64 == pubkey_b64 {
            return Ok(existing.clone());
        }
        return Err(PeersStoreError::PubkeyConflict {
            peer_id,
            stored_pubkey_b64: existing.pubkey_b64.clone(),
            attempted_pubkey_b64: pubkey_b64,
        });
    }

    let entry = StoredPeer {
        peer_id,
        pubkey_b64,
        added_at_ms: now_ms()?,
    };
    peers.push(entry.clone());
    save(home, &peers)?;
    Ok(entry)
}

/// Audit-log entry recording one applied rotation. Append-only;
/// inspectable via [`audit_log`]. Persisted in `peers_audit.jsonl`
/// adjacent to `peers.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RotationAuditEntry {
    pub peer_id: PeerId,
    pub prev_pubkey_b64: String,
    pub next_pubkey_b64: String,
    pub sequence: u64,
    /// Producer's `rotated_at_ms` from the rotation event. Audit-only.
    pub rotated_at_ms: u64,
    /// Local clock when the rotation was applied to this store.
    pub applied_at_ms: u64,
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
/// 4. Append an [`RotationAuditEntry`] to `peers_audit.jsonl`.
/// 5. Persist the new pubkey to `peers.json`.
///
/// Order matters: audit is appended BEFORE peers.json is rewritten.
/// If the audit write fails, the store is unchanged — rotation
/// didn't apply. If peers.json rewrite fails after audit succeeded,
/// the audit shows an intent that didn't land — `audit_log` and
/// the on-disk pubkey will disagree and a follow-up reconciliation
/// pass can detect that. Surfaced as `Io` either way, never silently.
pub fn rotate(home: &Path, rotation: &TrustRotation) -> Result<StoredPeer, PeersStoreError> {
    // Step 1: crypto.
    verify_rotation(rotation)?;

    // Step 2: stored prev_pubkey matches what rotation names.
    let mut peers = load(home)?;
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
    let last_seq = last_applied_sequence(home, rotation.peer_id)?;
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
    append_audit(home, &audit_entry)?;

    // Step 5: rewrite peers.json with the new pubkey.
    peers[stored_idx].pubkey_b64 = URL_SAFE_NO_PAD.encode(rotation.next_pubkey);
    peers[stored_idx].added_at_ms = applied_at_ms;
    save(home, &peers)?;
    Ok(peers[stored_idx].clone())
}

/// Read all audit entries for `peer_id` in apply-order. Returns an
/// empty vec if the audit log doesn't exist yet (no rotations have
/// ever been applied) or the peer has none.
pub fn audit_log(home: &Path, peer_id: PeerId) -> Result<Vec<RotationAuditEntry>, PeersStoreError> {
    let path = home.join(PEERS_AUDIT_FILENAME);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: RotationAuditEntry = serde_json::from_str(trimmed)?;
        if entry.peer_id == peer_id {
            out.push(entry);
        }
    }
    Ok(out)
}

fn last_applied_sequence(home: &Path, peer_id: PeerId) -> Result<u64, PeersStoreError> {
    Ok(audit_log(home, peer_id)?
        .into_iter()
        .map(|e| e.sequence)
        .max()
        .unwrap_or(0))
}

fn append_audit(home: &Path, entry: &RotationAuditEntry) -> Result<(), PeersStoreError> {
    use std::io::Write;
    std::fs::create_dir_all(home)?;
    let path = home.join(PEERS_AUDIT_FILENAME);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(entry)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

/// Write the peer list to disk, replacing the existing file.
pub fn save(home: &Path, peers: &[StoredPeer]) -> Result<(), PeersStoreError> {
    std::fs::create_dir_all(home)?;
    let path = path_in(home);
    let file = PeersFile {
        version: PEERS_VERSION,
        peers: peers.to_vec(),
    };
    let text = serde_json::to_string_pretty(&file)?;
    std::fs::write(&path, text)?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
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

    #[test]
    fn load_returns_empty_when_file_missing() {
        let home = TempDir::new().unwrap();
        let peers = load(home.path()).unwrap();
        assert!(peers.is_empty());
    }

    #[test]
    fn add_then_load_roundtrips() {
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let pk = fake_pubkey(0xaa);
        let stored = add(home.path(), id, pk).unwrap();
        assert_eq!(stored.peer_id, id);
        assert_eq!(stored.pubkey_bytes().unwrap(), pk);

        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].peer_id, id);
        assert_eq!(loaded[0].pubkey_bytes().unwrap(), pk);
    }

    #[test]
    fn add_is_idempotent_for_same_pubkey() {
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let pk = fake_pubkey(0xbb);
        let first = add(home.path(), id, pk).unwrap();
        let second = add(home.path(), id, pk).unwrap();
        // Same added_at_ms because second call returns the already-
        // stored entry (didn't overwrite).
        assert_eq!(first.added_at_ms, second.added_at_ms);
        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded.len(), 1, "duplicate enrolment must be deduped");
    }

    #[test]
    fn add_refuses_silent_overwrite_on_pubkey_conflict() {
        // Grievance §8 fix: silent overwrite on second `add` of the
        // same peer_id with a new pubkey is what the rotation path
        // replaces. `add` must now refuse and surface a typed error.
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let old = fake_pubkey(0xc0);
        let new = fake_pubkey(0xc1);
        add(home.path(), id, old).unwrap();
        let result = add(home.path(), id, new);
        assert!(
            matches!(result, Err(PeersStoreError::PubkeyConflict { peer_id, .. }) if peer_id == id),
            "second add() with a different pubkey must error PubkeyConflict; got {result:?}",
        );
        // Old pubkey must still be stored — no partial-overwrite.
        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].pubkey_bytes().unwrap(), old);
    }

    #[test]
    fn add_is_idempotent_for_same_peer_same_pubkey() {
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let key = fake_pubkey(0x42);
        let first = add(home.path(), id, key).unwrap();
        let second = add(home.path(), id, key).unwrap();
        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(first.pubkey_b64, second.pubkey_b64);
        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn signed_rotation_is_accepted_and_audited() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let prev_kp = PeerKeypair::generate();
        let next_kp = PeerKeypair::generate();

        add(home.path(), peer_id, prev_kp.public_bytes()).unwrap();

        let rotation = sign_rotation(
            &prev_kp,
            peer_id,
            next_kp.public_bytes(),
            1,
            1_700_000_000_000,
        )
        .unwrap();

        let updated = rotate(home.path(), &rotation).unwrap();
        assert_eq!(updated.pubkey_bytes().unwrap(), next_kp.public_bytes());

        // Audit trail visible to consumers.
        let audit = audit_log(home.path(), peer_id).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].sequence, 1);
        assert_eq!(audit[0].rotated_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn rotation_signed_by_wrong_key_is_rejected_before_audit() {
        use airc_protocol::{sign_rotation, PeerKeypair, RotationVerificationError};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let prev_kp = PeerKeypair::generate();
        let next_kp = PeerKeypair::generate();
        let imposter_kp = PeerKeypair::generate();

        add(home.path(), peer_id, prev_kp.public_bytes()).unwrap();

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

        let result = rotate(home.path(), &bad);
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
        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded[0].pubkey_bytes().unwrap(), prev_kp.public_bytes());
        assert!(audit_log(home.path(), peer_id).unwrap().is_empty());
    }

    #[test]
    fn stale_rotation_against_already_superseded_key_is_rejected() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let k0 = PeerKeypair::generate();
        let k1 = PeerKeypair::generate();
        let k2 = PeerKeypair::generate();

        add(home.path(), peer_id, k0.public_bytes()).unwrap();
        let r1 = sign_rotation(&k0, peer_id, k1.public_bytes(), 1, 1).unwrap();
        rotate(home.path(), &r1).unwrap();

        // Now the stored key is k1. Replay r1 (or any rotation whose
        // prev_pubkey is k0) must be rejected — k0 was superseded.
        let replay = sign_rotation(&k0, peer_id, k2.public_bytes(), 2, 2).unwrap();
        let result = rotate(home.path(), &replay);
        assert!(
            matches!(
                result,
                Err(PeersStoreError::PrevPubkeyMismatch { peer_id: p, .. }) if p == peer_id
            ),
            "rotation against a superseded prev_pubkey must error PrevPubkeyMismatch; got {result:?}",
        );
    }

    #[test]
    fn rotation_with_non_monotonic_sequence_is_rejected() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let k0 = PeerKeypair::generate();
        let k1 = PeerKeypair::generate();
        let k2 = PeerKeypair::generate();

        add(home.path(), peer_id, k0.public_bytes()).unwrap();
        let r1 = sign_rotation(&k0, peer_id, k1.public_bytes(), 5, 100).unwrap();
        rotate(home.path(), &r1).unwrap();

        // Try to apply a rotation with sequence == previous (replay) and
        // sequence < previous (out-of-order). Both must fail.
        let same_seq = sign_rotation(&k1, peer_id, k2.public_bytes(), 5, 200).unwrap();
        let lower_seq = sign_rotation(&k1, peer_id, k2.public_bytes(), 3, 200).unwrap();
        assert!(matches!(
            rotate(home.path(), &same_seq),
            Err(PeersStoreError::SequenceNotMonotonic {
                last_applied: 5,
                attempted: 5,
                ..
            })
        ));
        assert!(matches!(
            rotate(home.path(), &lower_seq),
            Err(PeersStoreError::SequenceNotMonotonic {
                last_applied: 5,
                attempted: 3,
                ..
            })
        ));
    }

    #[test]
    fn rotation_for_unknown_peer_is_rejected() {
        use airc_protocol::{sign_rotation, PeerKeypair};

        let home = TempDir::new().unwrap();
        let peer_id = PeerId::new();
        let prev_kp = PeerKeypair::generate();
        let next_kp = PeerKeypair::generate();

        // Note: NO add() — peer isn't enrolled.
        let rotation = sign_rotation(&prev_kp, peer_id, next_kp.public_bytes(), 1, 0).unwrap();
        let result = rotate(home.path(), &rotation);
        assert!(matches!(
            result,
            Err(PeersStoreError::UnknownPeer(p)) if p == peer_id
        ));
    }

    #[test]
    fn multiple_distinct_peers_accumulate() {
        let home = TempDir::new().unwrap();
        let a = (PeerId::new(), fake_pubkey(0x01));
        let b = (PeerId::new(), fake_pubkey(0x02));
        let c = (PeerId::new(), fake_pubkey(0x03));
        add(home.path(), a.0, a.1).unwrap();
        add(home.path(), b.0, b.1).unwrap();
        add(home.path(), c.0, c.1).unwrap();
        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded.len(), 3);
        let ids: Vec<PeerId> = loaded.iter().map(|p| p.peer_id).collect();
        assert!(ids.contains(&a.0));
        assert!(ids.contains(&b.0));
        assert!(ids.contains(&c.0));
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path()).unwrap();
        std::fs::write(path_in(home.path()), r#"{"version":999,"peers":[]}"#).unwrap();
        let result = load(home.path());
        assert!(matches!(
            result,
            Err(PeersStoreError::SchemaVersionMismatch { found: 999, .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn peers_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        add(home.path(), id, fake_pubkey(0x42)).unwrap();
        let mode = std::fs::metadata(path_in(home.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
