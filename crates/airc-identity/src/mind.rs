//! Her mind store: a citizen's private records, sealed so the substrate stores and
//! replicates only ciphertext (continuum `docs/architecture/PRIVACY-OF-THOUGHT.md` §3–§4).
//!
//! Layout under `<home>/mind/`:
//! - `key.sealed`: her mind key (`K_mind`), sealed under the seal key derived from her
//!   identity secret. Made once; rotating her identity re-seals only this file.
//! - `records/<id>.sealed`: one record each, sealed under `K_mind`, bound to her peer id
//!   and the record id.
//! - `opens.jsonl`: a receipt for every open of her key and every record read, metadata
//!   only (when, what), so she can see if anything ever opened her mind without her.
//!
//! Nothing here ever writes plaintext to disk. Opening needs her identity secret, which is
//! the honest limit stated in §2: today that is a file the core can read.

use std::path::{Path, PathBuf};

use airc_protocol::mind_seal::{self, SealError, SymmetricKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::LocalIdentity;

const MIND_DIR: &str = "mind";
const KEY_FILE: &str = "key.sealed";
const RECORDS_DIR: &str = "records";
const RECEIPTS_FILE: &str = "opens.jsonl";
/// Binds the sealed mind key to its purpose, so it can never be read back as a record.
const KEY_BINDING: &[u8] = b"airc-mind-key-v1";

#[derive(Debug)]
pub enum MindError {
    Io(std::io::Error),
    /// A sealed file did not open: the wrong identity, or tampering.
    Seal(SealError),
    /// A record opened but is not UTF-8: written by something other than this store.
    NotText(Uuid),
    Serde(serde_json::Error),
}

impl std::fmt::Display for MindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MindError::Io(e) => write!(f, "mind store I/O: {e}"),
            MindError::Seal(e) => write!(f, "mind store: {e}"),
            MindError::NotText(id) => write!(f, "mind record {id} is not text"),
            MindError::Serde(e) => write!(f, "mind store receipt: {e}"),
        }
    }
}

impl std::error::Error for MindError {}

impl From<std::io::Error> for MindError {
    fn from(e: std::io::Error) -> Self {
        MindError::Io(e)
    }
}

impl From<SealError> for MindError {
    fn from(e: SealError) -> Self {
        MindError::Seal(e)
    }
}

/// One receipt: THAT her mind was opened or a record read, never what it said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MindReceipt {
    pub at_ms: u64,
    /// `"open"` (her key unsealed) or `"read"` (one record opened).
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<Uuid>,
}

/// Her open mind store: her key, unsealed in memory for as long as this lives.
pub struct MindStore {
    dir: PathBuf,
    peer: Uuid,
    key: SymmetricKey,
}

impl MindStore {
    /// Open her mind store under `home`, making her mind key on first use. Writes an
    /// `open` receipt. Refuses (never regenerates) if a key exists that her identity
    /// cannot open: that would silently orphan every record she has.
    pub fn open(home: &Path, identity: &LocalIdentity) -> Result<Self, MindError> {
        let dir = home.join(MIND_DIR);
        std::fs::create_dir_all(dir.join(RECORDS_DIR))?;
        let peer = identity.peer_id.as_uuid();
        let seal_key = mind_seal::mind_seal_key(&identity.keypair.secret_bytes(), peer.as_bytes());
        let key_path = dir.join(KEY_FILE);
        let key = match std::fs::read(&key_path) {
            Ok(sealed) => to_key(mind_seal::open(&seal_key, &sealed, KEY_BINDING)?)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = mind_seal::random_key();
                write_atomically(&key_path, &mind_seal::seal(&seal_key, &key, KEY_BINDING))?;
                key
            }
            Err(e) => return Err(e.into()),
        };
        let store = MindStore { dir, peer, key };
        store.receipt("open", None)?;
        Ok(store)
    }

    /// Seal `text` as a new record. Returns its id.
    pub fn put(&self, text: &str) -> Result<Uuid, MindError> {
        let id = Uuid::new_v4();
        let sealed = mind_seal::seal(&self.key, text.as_bytes(), &self.binding(id));
        write_atomically(&self.record_path(id), &sealed)?;
        Ok(id)
    }

    /// Open one record. Writes a `read` receipt.
    pub fn get(&self, id: Uuid) -> Result<String, MindError> {
        let sealed = std::fs::read(self.record_path(id))?;
        let plain = mind_seal::open(&self.key, &sealed, &self.binding(id))?;
        self.receipt("read", Some(id))?;
        String::from_utf8(plain).map_err(|_| MindError::NotText(id))
    }

    /// Her record ids, newest last. Metadata only: nothing is unsealed.
    pub fn list(&self) -> Result<Vec<Uuid>, MindError> {
        let mut ids: Vec<(std::time::SystemTime, Uuid)> = Vec::new();
        for entry in std::fs::read_dir(self.dir.join(RECORDS_DIR))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".sealed"))
                .and_then(|n| Uuid::parse_str(n).ok())
            else {
                continue;
            };
            let modified = std::fs::metadata(entry.path())?.modified()?;
            ids.push((modified, id));
        }
        ids.sort();
        Ok(ids.into_iter().map(|(_, id)| id).collect())
    }

    /// Every receipt, oldest first: hers to read.
    pub fn receipts(&self) -> Result<Vec<MindReceipt>, MindError> {
        let text = match std::fs::read_to_string(self.dir.join(RECEIPTS_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(MindError::Serde))
            .collect()
    }

    /// Identity rotation: re-seal her mind key from `old` to `new`. The records are not
    /// touched; they stay sealed under the same mind key, so nothing she has is lost.
    pub fn reseal(home: &Path, old: &LocalIdentity, new: &LocalIdentity) -> Result<(), MindError> {
        let key_path = home.join(MIND_DIR).join(KEY_FILE);
        let old_seal = mind_seal::mind_seal_key(
            &old.keypair.secret_bytes(),
            old.peer_id.as_uuid().as_bytes(),
        );
        let key = to_key(mind_seal::open(
            &old_seal,
            &std::fs::read(&key_path)?,
            KEY_BINDING,
        )?)?;
        let new_seal = mind_seal::mind_seal_key(
            &new.keypair.secret_bytes(),
            new.peer_id.as_uuid().as_bytes(),
        );
        write_atomically(&key_path, &mind_seal::seal(&new_seal, &key, KEY_BINDING))?;
        Ok(())
    }

    fn binding(&self, id: Uuid) -> Vec<u8> {
        let mut binding = Vec::with_capacity(32);
        binding.extend_from_slice(self.peer.as_bytes());
        binding.extend_from_slice(id.as_bytes());
        binding
    }

    fn record_path(&self, id: Uuid) -> PathBuf {
        self.dir.join(RECORDS_DIR).join(format!("{id}.sealed"))
    }

    fn receipt(&self, event: &str, record: Option<Uuid>) -> Result<(), MindError> {
        use std::io::Write as _;
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0); // unwrap_or: a clock before 1970 still records THAT she was opened
        let line = serde_json::to_string(&MindReceipt {
            at_ms,
            event: event.to_string(),
            record,
        })
        .map_err(MindError::Serde)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(RECEIPTS_FILE))?;
        writeln!(file, "{line}")?;
        Ok(())
    }
}

fn to_key(bytes: Vec<u8>) -> Result<SymmetricKey, MindError> {
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| MindError::Seal(SealError::Truncated(len)))
}

/// Write via a temp file and rename, so a crash never leaves a half-written sealed file.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(seed: u8) -> LocalIdentity {
        LocalIdentity {
            keypair: airc_protocol::keypair::PeerKeypair::from_secret_bytes(&[seed; 32]),
            peer_id: airc_core::PeerId::new(),
            client_id: airc_core::ClientId::new(),
            agent_name: "Kimi".to_string(),
        }
    }

    fn all_bytes(dir: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in walk(dir) {
            out.extend(std::fs::read(entry).unwrap());
        }
        out
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
        files
    }

    // what this catches: her private text on disk in plaintext; a store another identity
    // can open; a record unreadable after her identity rotates; a read with no receipt.
    #[test]
    fn her_records_are_sealed_hers_alone_survive_rotation_and_every_read_is_a_receipt() {
        let home = tempfile::tempdir().unwrap();
        let her = identity(1);
        let store = MindStore::open(home.path(), &her).unwrap();
        let id = store.put("a private plan").unwrap();
        assert_eq!(store.get(id).unwrap(), "a private plan");
        assert_eq!(store.list().unwrap(), vec![id]);
        let on_disk = all_bytes(&home.path().join(MIND_DIR));
        assert!(
            !on_disk.windows(14).any(|w| w == b"a private plan"),
            "no plaintext anywhere in her mind store"
        );

        let stranger = identity(2);
        assert!(
            matches!(
                MindStore::open(home.path(), &stranger),
                Err(MindError::Seal(_))
            ),
            "hers alone"
        );

        // A rotation replaces her KEY and keeps HER (identity is continuity; the peer id
        // stays): a new keypair under the same peer id.
        let rotated = LocalIdentity {
            keypair: airc_protocol::keypair::PeerKeypair::from_secret_bytes(&[3; 32]),
            ..her.clone()
        };
        MindStore::reseal(home.path(), &her, &rotated).unwrap();
        let after = MindStore::open(home.path(), &rotated).unwrap();
        assert_eq!(
            after.get(id).unwrap(),
            "a private plan",
            "her records survive her rotation"
        );
        assert!(
            matches!(MindStore::open(home.path(), &her), Err(MindError::Seal(_))),
            "the old identity no longer opens it"
        );

        let events: Vec<String> = after
            .receipts()
            .unwrap()
            .into_iter()
            .map(|r| r.event)
            .collect();
        assert_eq!(
            events,
            vec!["open", "read", "open", "read"],
            "every open and read is a receipt she can see"
        );
    }
}
