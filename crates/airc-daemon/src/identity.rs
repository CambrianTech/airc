//! Persisted local identity — `LocalIdentity` bundles the keypair + a
//! stable `peer_id` + `client_id` so the substrate's identity survives
//! across CLI invocations and across daemon restarts.
//!
//! Layout (under `<home>/`, default `$HOME/.airc/`):
//!
//!   - `identity.key`  — raw 32-byte Ed25519 secret (0600 on Unix).
//!     Secret material stays in a file because secrets at rest belong
//!     in OS-protected storage (filesystem perms, keychain,
//!     SQLCipher, hardware enclave) — not inlined into a database
//!     row that may be backed up, replicated, or inspected with a
//!     generic SQL browser. The substrate rule: blobs on disk, never
//!     in DB.
//!
//!   - `events.sqlite::local_identity` — singleton ORM row holding
//!     the metadata that used to live in `identity.json`
//!     (`peer_id`, `client_id`, schema version, created-at). Pairs
//!     with the on-disk key.
//!
//! The two are paired. `load_or_generate` treats them as one unit:
//! either both exist (load), or both are absent (generate + save).
//! One present + one absent is an error — we refuse to recover
//! ambiguously because losing either half changes peer identity.
//!
//! Storage caveat: `identity.key` is plain on-disk material. A
//! production deployment belongs behind SQLCipher / OS keychain /
//! hardware enclave. The 0600-file storage is adequate for
//! cross-process demos + the current daemon use case; it's
//! deliberately replaceable.

use std::path::{Path, PathBuf};

use airc_core::{identity::Identity, ClientId, PeerId};
use airc_protocol::PeerKeypair;
use airc_store::{SqliteEventStore, StoreError, StoredLocalIdentity};

/// On-disk schema version for the singleton row. Bump when the
/// stored shape changes incompatibly.
const IDENTITY_STATE_VERSION: u32 = 1;

const IDENTITY_KEY_FILENAME: &str = "identity.key";
const STORE_DB_FILENAME: &str = "events.sqlite";

/// Persisted-state bundle: stable identity for this airc install.
#[derive(Debug, Clone)]
pub struct LocalIdentity {
    pub keypair: PeerKeypair,
    pub peer_id: PeerId,
    pub client_id: ClientId,
}

#[derive(Debug)]
pub enum IdentityError {
    Io(std::io::Error),
    Store(StoreError),
    /// Persisted row's `version` doesn't match what this build knows
    /// how to read. Refuse to load rather than silently reinterpret.
    SchemaVersionMismatch {
        found: u32,
        expected: u32,
    },
    /// `identity.key` exists but the paired singleton row is missing
    /// (or vice versa). The pair is required to disambiguate
    /// identity — refusing rather than regenerating prevents silent
    /// peer-id rotation.
    PartialState {
        key_exists: bool,
        state_exists: bool,
    },
    /// `identity.key` exists but isn't the expected 32 bytes.
    BadKeyLength(usize),
    /// System clock is before UNIX_EPOCH; refuse to write a corrupt
    /// timestamp into the identity audit record.
    Clock(std::time::SystemTimeError),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::Io(error) => write!(f, "identity I/O: {error}"),
            IdentityError::Store(error) => write!(f, "identity store: {error}"),
            IdentityError::SchemaVersionMismatch { found, expected } => {
                write!(
                    f,
                    "local_identity row schema version {found}, this build expects {expected}"
                )
            }
            IdentityError::PartialState {
                key_exists,
                state_exists,
            } => write!(
                f,
                "identity is half-initialised: key={} state={}. \
                 Either remove the remaining file/row to regenerate, or restore the missing half — \
                 the substrate refuses to invent a new peer_id over an existing key.",
                key_exists, state_exists
            ),
            IdentityError::BadKeyLength(got) => write!(
                f,
                "identity.key is {got} bytes, expected 32 (raw Ed25519 secret)"
            ),
            IdentityError::Clock(error) => write!(f, "identity timestamp clock error: {error}"),
        }
    }
}

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IdentityError::Io(error) => Some(error),
            IdentityError::Store(error) => Some(error),
            IdentityError::Clock(error) => Some(error),
            IdentityError::SchemaVersionMismatch { .. }
            | IdentityError::PartialState { .. }
            | IdentityError::BadKeyLength(_) => None,
        }
    }
}

impl From<std::io::Error> for IdentityError {
    fn from(error: std::io::Error) -> Self {
        IdentityError::Io(error)
    }
}

impl From<StoreError> for IdentityError {
    fn from(error: StoreError) -> Self {
        IdentityError::Store(error)
    }
}

impl From<std::time::SystemTimeError> for IdentityError {
    fn from(error: std::time::SystemTimeError) -> Self {
        IdentityError::Clock(error)
    }
}

impl LocalIdentity {
    /// Path to the secret key file inside `home`. Secret material
    /// lives in a file with 0600 perms on Unix; this stays out of
    /// the ORM database (see module doc).
    pub fn key_path(home: &Path) -> PathBuf {
        home.join(IDENTITY_KEY_FILENAME)
    }

    /// Load the existing identity from `home`, or generate a fresh
    /// one (writing both halves) if neither exists. Refuses to
    /// recover from a half-initialised state.
    pub async fn load_or_generate(home: &Path) -> Result<Self, IdentityError> {
        let store = open_store(home).await?;
        let key_path = Self::key_path(home);
        let key_exists = key_path.exists();
        let stored = store.load_local_identity().await?;
        match (key_exists, stored) {
            (true, Some(stored)) => Self::load_with_metadata(home, stored),
            (false, None) => Self::generate_and_save(home, &store).await,
            (key, state) => Err(IdentityError::PartialState {
                key_exists: key,
                state_exists: state.is_some(),
            }),
        }
    }

    /// Load an existing identity (both halves must exist). Public
    /// for the cases that already know they have a populated
    /// install and want to fail loudly if either half is missing.
    pub async fn load(home: &Path) -> Result<Self, IdentityError> {
        let store = open_store(home).await?;
        let stored = store
            .load_local_identity()
            .await?
            .ok_or(IdentityError::PartialState {
                key_exists: Self::key_path(home).exists(),
                state_exists: false,
            })?;
        Self::load_with_metadata(home, stored)
    }

    fn load_with_metadata(home: &Path, stored: StoredLocalIdentity) -> Result<Self, IdentityError> {
        if stored.version != IDENTITY_STATE_VERSION {
            return Err(IdentityError::SchemaVersionMismatch {
                found: stored.version,
                expected: IDENTITY_STATE_VERSION,
            });
        }
        let key_bytes = std::fs::read(Self::key_path(home))?;
        if key_bytes.len() != 32 {
            return Err(IdentityError::BadKeyLength(key_bytes.len()));
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&key_bytes);
        let keypair = PeerKeypair::from_secret_bytes(&secret);
        Ok(Self {
            keypair,
            peer_id: stored.peer_id,
            client_id: stored.client_id,
        })
    }

    /// Generate a fresh identity + persist it. Writes the secret
    /// key file (0600) and inserts the singleton row in one logical
    /// step; if the row insert fails after the key was written, the
    /// caller can clean up by removing `identity.key` and retrying.
    pub async fn generate_and_save(
        home: &Path,
        store: &SqliteEventStore,
    ) -> Result<Self, IdentityError> {
        ensure_home_dir(home)?;
        let keypair = PeerKeypair::generate();
        let peer_id = PeerId::new();
        let client_id = ClientId::new();
        let created_at_ms = now_ms()?;

        write_owner_only(&Self::key_path(home), &keypair.secret_bytes())?;

        store
            .insert_local_identity(StoredLocalIdentity {
                peer_id,
                client_id,
                version: IDENTITY_STATE_VERSION,
                created_at_ms,
                identity: Identity::default(),
            })
            .await?;

        Ok(Self {
            keypair,
            peer_id,
            client_id,
        })
    }
}

async fn open_store(home: &Path) -> Result<SqliteEventStore, IdentityError> {
    if let Some(parent) = home.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(home)?;
        }
    } else {
        std::fs::create_dir_all(home)?;
    }
    Ok(SqliteEventStore::open_path(&home.join(STORE_DB_FILENAME)).await?)
}

/// Create `home` if missing; on Unix, restrict to 0700 (owner-only)
/// so the secret file isn't even discoverable by other users on the
/// same machine.
fn ensure_home_dir(home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(home)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(home, perms)?;
    }
    Ok(())
}

/// Write a file with 0600 (owner-only) permissions on Unix. Windows
/// inherits the user-profile ACL — call out as a known cross-platform
/// gap for the production-hardening pass.
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
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

    #[tokio::test]
    async fn load_or_generate_creates_both_halves_and_reuses_them() {
        let home = TempDir::new().unwrap();
        let first = LocalIdentity::load_or_generate(home.path()).await.unwrap();
        assert!(LocalIdentity::key_path(home.path()).exists());

        let second = LocalIdentity::load_or_generate(home.path()).await.unwrap();
        // The crucial invariant: same key, same peer_id across runs.
        assert_eq!(first.keypair.secret_bytes(), second.keypair.secret_bytes());
        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(first.client_id, second.client_id);
    }

    #[tokio::test]
    async fn refuses_partial_state_when_only_key_exists() {
        // Key without row would silently regenerate a fresh peer_id
        // if we tolerated it — that would break every prior peer
        // who'd already enrolled the old peer_id. Refuse.
        let home = TempDir::new().unwrap();
        write_owner_only(&LocalIdentity::key_path(home.path()), &[0u8; 32]).unwrap();
        // Open + close the store so its file exists but the
        // singleton row does not.
        let _store = open_store(home.path()).await.unwrap();
        let result = LocalIdentity::load_or_generate(home.path()).await;
        assert!(matches!(
            result,
            Err(IdentityError::PartialState {
                key_exists: true,
                state_exists: false,
            })
        ));
    }

    #[tokio::test]
    async fn refuses_partial_state_when_only_row_exists() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path()).unwrap();
        let store = open_store(home.path()).await.unwrap();
        store
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::new(),
                client_id: ClientId::new(),
                version: IDENTITY_STATE_VERSION,
                created_at_ms: 1,
                identity: Identity::default(),
            })
            .await
            .unwrap();
        let result = LocalIdentity::load_or_generate(home.path()).await;
        assert!(matches!(
            result,
            Err(IdentityError::PartialState {
                key_exists: false,
                state_exists: true,
            })
        ));
    }

    #[tokio::test]
    async fn refuses_bad_key_length() {
        let home = TempDir::new().unwrap();
        let identity = LocalIdentity::load_or_generate(home.path()).await.unwrap();
        // Corrupt the key file.
        std::fs::write(LocalIdentity::key_path(home.path()), b"too short").unwrap();
        let result = LocalIdentity::load(home.path()).await;
        assert!(matches!(result, Err(IdentityError::BadKeyLength(_))));
        // The valid identity we generated above is still good — pin
        // that load failure doesn't corrupt the in-memory copy.
        assert_eq!(identity.peer_id.to_string().len(), 36);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn key_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().unwrap();
        LocalIdentity::load_or_generate(home.path()).await.unwrap();
        let key = LocalIdentity::key_path(home.path());
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{} not 0600", key.display());
    }
}
