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

const PEERS_FILENAME: &str = "peers.json";
const PEERS_VERSION: u32 = 1;

#[derive(Debug)]
pub enum PeersStoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    Clock(std::time::SystemTimeError),
    SchemaVersionMismatch { found: u32, expected: u32 },
    WrongPubkeyLength(usize),
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
            PeersStoreError::SchemaVersionMismatch { .. }
            | PeersStoreError::WrongPubkeyLength(_) => None,
        }
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

/// Add a peer to the persisted list. Idempotent — adding a peer
/// already present (same peer_id + pubkey) is a no-op. Different
/// pubkey for an existing peer_id REPLACES (treat as rotation).
pub fn add(home: &Path, peer_id: PeerId, pubkey: [u8; 32]) -> Result<StoredPeer, PeersStoreError> {
    let mut peers = load(home)?;
    let pubkey_b64 = URL_SAFE_NO_PAD.encode(pubkey);
    let entry = StoredPeer {
        peer_id,
        pubkey_b64: pubkey_b64.clone(),
        added_at_ms: now_ms()?,
    };

    // Replace existing entry for this peer_id (rotation) or append.
    if let Some(existing) = peers.iter_mut().find(|p| p.peer_id == peer_id) {
        if existing.pubkey_b64 == pubkey_b64 {
            // Identical entry — idempotent no-op, return the
            // already-stored version.
            return Ok(existing.clone());
        }
        *existing = entry.clone();
    } else {
        peers.push(entry.clone());
    }

    save(home, &peers)?;
    Ok(entry)
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
    fn add_replaces_existing_entry_on_pubkey_rotation() {
        // Rotation case: same peer_id, new pubkey → overwrite.
        let home = TempDir::new().unwrap();
        let id = PeerId::new();
        let old = fake_pubkey(0xc0);
        let new = fake_pubkey(0xc1);
        add(home.path(), id, old).unwrap();
        add(home.path(), id, new).unwrap();
        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].pubkey_bytes().unwrap(),
            new,
            "later add must overwrite earlier pubkey for the same peer_id"
        );
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
