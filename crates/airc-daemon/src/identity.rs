//! Persisted local identity — `LocalIdentity` bundles the keypair + a
//! stable `peer_id` + `client_id` so the substrate's identity survives
//! across CLI invocations and across daemon restarts.
//!
//! Layout on disk (under `<home>/`, default `$HOME/.airc/`):
//!
//!   - `identity.key`  — raw 32-byte Ed25519 secret (0600)
//!   - `identity.json` — `{ peer_id, client_id, version, created_at_ms }` (0600)
//!
//! The two files are paired. `load_or_generate` treats them as one
//! unit: either both exist (load), or both are absent (generate +
//! save). One present + one absent is an error — we refuse to recover
//! ambiguously because losing either half changes peer identity.
//!
//! Storage caveat: this is plain on-disk material. A production
//! deployment belongs behind SQLCipher / OS keychain / hardware
//! enclave per the substrate `feedback_blobs_never_in_db` rule. The
//! CLI's file storage is adequate for cross-process demos + the
//! current daemon use case; it's deliberately replaceable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use airc_core::{ClientId, PeerId};
use airc_protocol::PeerKeypair;

/// On-disk schema version for `identity.json`. Bump when the file
/// shape changes incompatibly.
const IDENTITY_STATE_VERSION: u32 = 1;

/// Sibling metadata file alongside `identity.key`.
const IDENTITY_KEY_FILENAME: &str = "identity.key";
const IDENTITY_STATE_FILENAME: &str = "identity.json";

/// Persisted-state bundle: stable identity for this airc-rs install.
#[derive(Debug, Clone)]
pub struct LocalIdentity {
    pub keypair: PeerKeypair,
    pub peer_id: PeerId,
    pub client_id: ClientId,
}

#[derive(Debug, Serialize, Deserialize)]
struct IdentityState {
    version: u32,
    peer_id: PeerId,
    client_id: ClientId,
    created_at_ms: u64,
}

#[derive(Debug)]
pub enum IdentityError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// Identity-state file's `version` doesn't match what this build
    /// knows how to read. Refuse to load rather than silently
    /// reinterpret.
    SchemaVersionMismatch {
        found: u32,
        expected: u32,
    },
    /// `identity.key` exists but the sibling `identity.json` doesn't
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
            IdentityError::Json(error) => write!(f, "identity state JSON: {error}"),
            IdentityError::SchemaVersionMismatch { found, expected } => {
                write!(
                    f,
                    "identity.json schema version {found}, this build expects {expected}"
                )
            }
            IdentityError::PartialState {
                key_exists,
                state_exists,
            } => write!(
                f,
                "identity is half-initialised: key={} state={}. \
                 Either remove both files to regenerate, or restore the missing one — \
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
            IdentityError::Json(error) => Some(error),
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

impl From<serde_json::Error> for IdentityError {
    fn from(error: serde_json::Error) -> Self {
        IdentityError::Json(error)
    }
}

impl From<std::time::SystemTimeError> for IdentityError {
    fn from(error: std::time::SystemTimeError) -> Self {
        IdentityError::Clock(error)
    }
}

impl LocalIdentity {
    /// Path to the secret key file inside `home`.
    pub fn key_path(home: &Path) -> PathBuf {
        home.join(IDENTITY_KEY_FILENAME)
    }

    /// Path to the state file inside `home`.
    pub fn state_path(home: &Path) -> PathBuf {
        home.join(IDENTITY_STATE_FILENAME)
    }

    /// Load the existing identity from `home`, or generate a fresh
    /// one (writing both files) if neither exists. Refuses to
    /// recover from a half-initialised state.
    pub fn load_or_generate(home: &Path) -> Result<Self, IdentityError> {
        let key_path = Self::key_path(home);
        let state_path = Self::state_path(home);
        let key_exists = key_path.exists();
        let state_exists = state_path.exists();
        match (key_exists, state_exists) {
            (true, true) => Self::load(home),
            (false, false) => Self::generate_and_save(home),
            (key, state) => Err(IdentityError::PartialState {
                key_exists: key,
                state_exists: state,
            }),
        }
    }

    /// Load an existing identity (both files must exist).
    pub fn load(home: &Path) -> Result<Self, IdentityError> {
        let key_bytes = std::fs::read(Self::key_path(home))?;
        if key_bytes.len() != 32 {
            return Err(IdentityError::BadKeyLength(key_bytes.len()));
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&key_bytes);
        let keypair = PeerKeypair::from_secret_bytes(&secret);

        let state_text = std::fs::read_to_string(Self::state_path(home))?;
        let state: IdentityState = serde_json::from_str(&state_text)?;
        if state.version != IDENTITY_STATE_VERSION {
            return Err(IdentityError::SchemaVersionMismatch {
                found: state.version,
                expected: IDENTITY_STATE_VERSION,
            });
        }
        Ok(Self {
            keypair,
            peer_id: state.peer_id,
            client_id: state.client_id,
        })
    }

    /// Generate a fresh identity + persist it under `home`. Returns
    /// the same `LocalIdentity` a subsequent `load(home)` would
    /// produce.
    pub fn generate_and_save(home: &Path) -> Result<Self, IdentityError> {
        ensure_home_dir(home)?;
        let keypair = PeerKeypair::generate();
        let peer_id = PeerId::new();
        let client_id = ClientId::new();
        let created_at_ms = now_ms()?;

        write_owner_only(&Self::key_path(home), &keypair.secret_bytes())?;

        let state = IdentityState {
            version: IDENTITY_STATE_VERSION,
            peer_id,
            client_id,
            created_at_ms,
        };
        let state_text = serde_json::to_string_pretty(&state)?;
        write_owner_only(&Self::state_path(home), state_text.as_bytes())?;

        Ok(Self {
            keypair,
            peer_id,
            client_id,
        })
    }
}

/// Create `home` if missing; on Unix, restrict to 0700 (owner-only)
/// so the secret + state files aren't even discoverable by other
/// users on the same machine.
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

    #[test]
    fn load_or_generate_creates_both_files_and_reuses_them() {
        let home = TempDir::new().unwrap();
        let first = LocalIdentity::load_or_generate(home.path()).unwrap();
        assert!(LocalIdentity::key_path(home.path()).exists());
        assert!(LocalIdentity::state_path(home.path()).exists());

        let second = LocalIdentity::load_or_generate(home.path()).unwrap();
        // The crucial invariant: same key, same peer_id across runs.
        assert_eq!(first.keypair.secret_bytes(), second.keypair.secret_bytes());
        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(first.client_id, second.client_id);
    }

    #[test]
    fn refuses_partial_state_when_only_key_exists() {
        // Key without state would silently regenerate a fresh peer_id
        // if we tolerated it — that would break every prior peer
        // who'd already enrolled the old peer_id. Refuse.
        let home = TempDir::new().unwrap();
        write_owner_only(&LocalIdentity::key_path(home.path()), &[0u8; 32]).unwrap();
        let result = LocalIdentity::load_or_generate(home.path());
        assert!(matches!(
            result,
            Err(IdentityError::PartialState {
                key_exists: true,
                state_exists: false,
            })
        ));
    }

    #[test]
    fn refuses_partial_state_when_only_json_exists() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path()).unwrap();
        std::fs::write(LocalIdentity::state_path(home.path()), "{}").unwrap();
        let result = LocalIdentity::load_or_generate(home.path());
        assert!(matches!(
            result,
            Err(IdentityError::PartialState {
                key_exists: false,
                state_exists: true,
            })
        ));
    }

    #[test]
    fn refuses_bad_key_length() {
        let home = TempDir::new().unwrap();
        let identity = LocalIdentity::generate_and_save(home.path()).unwrap();
        // Corrupt the key file.
        std::fs::write(LocalIdentity::key_path(home.path()), b"too short").unwrap();
        let result = LocalIdentity::load(home.path());
        assert!(matches!(result, Err(IdentityError::BadKeyLength(_))));
        // The valid identity we generated above is still good — pin
        // that load failure doesn't corrupt the in-memory copy.
        assert_eq!(identity.peer_id.to_string().len(), 36);
    }

    #[test]
    fn refuses_unknown_schema_version() {
        let home = TempDir::new().unwrap();
        LocalIdentity::generate_and_save(home.path()).unwrap();
        std::fs::write(
            LocalIdentity::state_path(home.path()),
            r#"{"version":999,"peer_id":"00000000-0000-0000-0000-000000000001","client_id":"00000000-0000-0000-0000-000000000002","created_at_ms":0}"#,
        )
        .unwrap();
        let result = LocalIdentity::load(home.path());
        assert!(matches!(
            result,
            Err(IdentityError::SchemaVersionMismatch {
                found: 999,
                expected: IDENTITY_STATE_VERSION,
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn key_and_state_files_are_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempDir::new().unwrap();
        LocalIdentity::generate_and_save(home.path()).unwrap();
        for file in [
            LocalIdentity::key_path(home.path()),
            LocalIdentity::state_path(home.path()),
        ] {
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            // Strip the file-type bits, keep just the perm bits.
            assert_eq!(mode & 0o777, 0o600, "{} not 0600", file.display());
        }
    }
}
