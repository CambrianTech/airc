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
use serde::Deserialize;

/// On-disk schema version for local identity rows. Bump when the
/// stored shape changes incompatibly.
const IDENTITY_STATE_VERSION: u32 = 1;

/// Environment override for selecting the local agent identity row.
/// Card 8384cc18 Sub-D pairs this with `airc init --as <name>`.
pub const AIRC_AGENT_NAME_ENV: &str = "AIRC_AGENT_NAME";

const IDENTITY_KEY_FILENAME: &str = "identity.key";
const LEGACY_IDENTITY_JSON_FILENAME: &str = "identity.json";
const STORE_DB_FILENAME: &str = "events.sqlite";

/// Persisted-state bundle: stable identity for this airc install.
#[derive(Debug, Clone)]
pub struct LocalIdentity {
    pub keypair: PeerKeypair,
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub agent_name: String,
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
    /// `identity.key` exists but the paired local-identity row is missing
    /// (or vice versa). The pair is required to disambiguate
    /// identity — refusing rather than regenerating prevents silent
    /// peer-id rotation.
    PartialState {
        key_exists: bool,
        state_exists: bool,
    },
    /// `identity.key` exists but isn't the expected 32 bytes.
    BadKeyLength(usize),
    /// A prior install has `identity.json`, but it is not valid
    /// enough to deterministically migrate into the ORM row.
    LegacyIdentityJson(serde_json::Error),
    /// System clock is before UNIX_EPOCH; refuse to write a corrupt
    /// timestamp into the identity audit record.
    Clock(std::time::SystemTimeError),
    /// Requested local agent names are persisted as stable keys, so
    /// they must be explicit and printable.
    InvalidAgentName(String),
    /// A caller requested one local agent, but the only row available
    /// through the current store API describes another. Sub-C replaces
    /// the singleton read with lookup-by-agent-name.
    AgentNameMismatch {
        requested: String,
        found: String,
    },
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
            IdentityError::LegacyIdentityJson(error) => {
                write!(
                    f,
                    "identity.json could not be migrated into local_identity: {error}"
                )
            }
            IdentityError::Clock(error) => write!(f, "identity timestamp clock error: {error}"),
            IdentityError::InvalidAgentName(value) => write!(
                f,
                "invalid agent name {value:?}; use a non-empty printable name"
            ),
            IdentityError::AgentNameMismatch { requested, found } => write!(
                f,
                "requested local agent {requested:?}, but stored local_identity row is {found:?}"
            ),
        }
    }
}

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IdentityError::Io(error) => Some(error),
            IdentityError::Store(error) => Some(error),
            IdentityError::LegacyIdentityJson(error) => Some(error),
            IdentityError::Clock(error) => Some(error),
            IdentityError::SchemaVersionMismatch { .. }
            | IdentityError::PartialState { .. }
            | IdentityError::BadKeyLength(_)
            | IdentityError::InvalidAgentName(_)
            | IdentityError::AgentNameMismatch { .. } => None,
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
        let agent_name = requested_agent_name(None)?;
        Self::load_or_generate_as(home, agent_name).await
    }

    /// Load or generate the local identity for an explicit agent name.
    /// This is the programmatic partner of `airc init --as <name>`.
    pub async fn load_or_generate_as(
        home: &Path,
        agent_name: impl AsRef<str>,
    ) -> Result<Self, IdentityError> {
        let agent_name = normalise_agent_name(agent_name.as_ref())?;
        let store = open_store(home).await?;
        let key_path = Self::key_path(home);
        let key_exists = key_path.exists();
        let stored = store.load_local_identity_by_agent_name(&agent_name).await?;
        match (key_exists, stored) {
            (true, Some(stored)) => Self::load_with_metadata_for_agent(home, stored, &agent_name),
            (true, None) => match migrate_legacy_identity_json(home, &store).await? {
                Some(stored) if stored.agent_name == agent_name => {
                    Self::load_with_metadata(home, stored)
                }
                Some(_) => {
                    Self::generate_and_save_agent_with_existing_key(home, &store, &agent_name).await
                }
                None if store.has_local_identity_rows().await? => {
                    Self::generate_and_save_agent_with_existing_key(home, &store, &agent_name).await
                }
                None => Err(IdentityError::PartialState {
                    key_exists: true,
                    state_exists: false,
                }),
            },
            (false, None) => Self::generate_and_save_as(home, &store, &agent_name).await,
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
        let agent_name = requested_agent_name(None)?;
        let store = open_store(home).await?;
        let stored = store
            .load_local_identity_by_agent_name(&agent_name)
            .await?
            .ok_or(IdentityError::PartialState {
                key_exists: Self::key_path(home).exists(),
                state_exists: false,
            })?;
        Self::load_with_metadata(home, stored)
    }

    fn load_with_metadata_for_agent(
        home: &Path,
        stored: StoredLocalIdentity,
        requested_agent_name: &str,
    ) -> Result<Self, IdentityError> {
        if stored.agent_name != requested_agent_name {
            return Err(IdentityError::AgentNameMismatch {
                requested: requested_agent_name.to_string(),
                found: stored.agent_name,
            });
        }
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
            agent_name: stored.agent_name,
        })
    }

    async fn generate_and_save_agent_with_existing_key(
        home: &Path,
        store: &SqliteEventStore,
        agent_name: &str,
    ) -> Result<Self, IdentityError> {
        let key_bytes = std::fs::read(Self::key_path(home))?;
        if key_bytes.len() != 32 {
            return Err(IdentityError::BadKeyLength(key_bytes.len()));
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&key_bytes);
        let keypair = PeerKeypair::from_secret_bytes(&secret);
        let peer_id = PeerId::new();
        let client_id = ClientId::new();
        let created_at_ms = now_ms()?;

        // Seed the published card's nick from a NAMED citizen's
        // agent_name (e.g. `attach_as("Claude")` / `init --as Claude`),
        // so the very first card it publishes carries "Claude" rather
        // than an empty name that resolves to a raw uuid for peers. The
        // DEFAULT scope keeps an empty name — that identity is the
        // user's, filled in later via `airc identity set`, and "default"
        // is a discriminator, not a display name.
        let identity = if agent_name == airc_store::DEFAULT_AGENT_NAME {
            Identity::default()
        } else {
            Identity::new(agent_name)
        };

        store
            .insert_local_identity(StoredLocalIdentity {
                peer_id,
                client_id,
                version: IDENTITY_STATE_VERSION,
                created_at_ms,
                identity,
                agent_name: agent_name.to_string(),
            })
            .await?;

        Ok(Self {
            keypair,
            peer_id,
            client_id,
            agent_name: agent_name.to_string(),
        })
    }

    /// Generate a fresh identity + persist it. Writes the secret
    /// key file (0600) and inserts the default-agent row in one
    /// logical step; if the row insert fails after the key was
    /// written, the caller can clean up by removing `identity.key`
    /// and retrying.
    pub async fn generate_and_save(
        home: &Path,
        store: &SqliteEventStore,
    ) -> Result<Self, IdentityError> {
        Self::generate_and_save_as(home, store, airc_store::DEFAULT_AGENT_NAME).await
    }

    pub async fn generate_and_save_as(
        home: &Path,
        store: &SqliteEventStore,
        agent_name: impl AsRef<str>,
    ) -> Result<Self, IdentityError> {
        let agent_name = normalise_agent_name(agent_name.as_ref())?;
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
                agent_name: agent_name.clone(),
            })
            .await?;

        Ok(Self {
            keypair,
            peer_id,
            client_id,
            agent_name,
        })
    }
}

#[derive(Debug, Deserialize)]
struct LegacyIdentityJson {
    version: u32,
    peer_id: PeerId,
    client_id: ClientId,
    created_at_ms: u64,
}

async fn migrate_legacy_identity_json(
    home: &Path,
    store: &SqliteEventStore,
) -> Result<Option<StoredLocalIdentity>, IdentityError> {
    let path = home.join(LEGACY_IDENTITY_JSON_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let parsed: LegacyIdentityJson = serde_json::from_slice(&std::fs::read(&path)?)
        .map_err(IdentityError::LegacyIdentityJson)?;
    if parsed.version != IDENTITY_STATE_VERSION {
        return Err(IdentityError::SchemaVersionMismatch {
            found: parsed.version,
            expected: IDENTITY_STATE_VERSION,
        });
    }
    let stored = StoredLocalIdentity {
        peer_id: parsed.peer_id,
        client_id: parsed.client_id,
        version: parsed.version,
        created_at_ms: parsed.created_at_ms,
        identity: Identity::default(),
        agent_name: airc_store::DEFAULT_AGENT_NAME.to_string(),
    };
    store.insert_local_identity(stored.clone()).await?;
    let _ = std::fs::remove_file(path);
    Ok(Some(stored))
}

fn requested_agent_name(explicit: Option<&str>) -> Result<String, IdentityError> {
    if let Some(value) = explicit {
        return normalise_agent_name(value);
    }
    match std::env::var_os(AIRC_AGENT_NAME_ENV) {
        Some(value) => normalise_agent_name(&value.to_string_lossy()),
        None => Ok(airc_store::DEFAULT_AGENT_NAME.to_string()),
    }
}

fn normalise_agent_name(value: &str) -> Result<String, IdentityError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return Err(IdentityError::InvalidAgentName(value.to_string()));
    }
    Ok(trimmed.to_string())
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
        assert_eq!(first.agent_name, airc_store::DEFAULT_AGENT_NAME);
    }

    #[tokio::test]
    async fn load_or_generate_as_records_agent_name() {
        let home = TempDir::new().unwrap();
        let identity = LocalIdentity::load_or_generate_as(home.path(), "codex")
            .await
            .unwrap();

        assert_eq!(identity.agent_name, "codex");
        let store = open_store(home.path()).await.unwrap();
        let stored = store
            .load_local_identity_by_agent_name("codex")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.agent_name, "codex");
        assert!(store.load_local_identity().await.unwrap().is_none());

        let second = LocalIdentity::load_or_generate_as(home.path(), "codex")
            .await
            .unwrap();
        assert_eq!(second.peer_id, identity.peer_id);
        assert_eq!(second.agent_name, "codex");

        let default = LocalIdentity::load_or_generate(home.path()).await.unwrap();
        assert_eq!(default.agent_name, airc_store::DEFAULT_AGENT_NAME);
        assert_ne!(default.peer_id, identity.peer_id);
        assert_eq!(
            default.keypair.secret_bytes(),
            identity.keypair.secret_bytes()
        );
    }

    #[tokio::test]
    async fn load_or_generate_as_adds_named_agent_to_existing_scope() {
        let home = TempDir::new().unwrap();
        let default = LocalIdentity::load_or_generate(home.path()).await.unwrap();
        let codex = LocalIdentity::load_or_generate_as(home.path(), "codex")
            .await
            .unwrap();

        assert_eq!(codex.agent_name, "codex");
        assert_ne!(default.peer_id, codex.peer_id);
        assert_ne!(default.client_id, codex.client_id);
        assert_eq!(default.keypair.secret_bytes(), codex.keypair.secret_bytes());

        let store = open_store(home.path()).await.unwrap();
        let default_row = store.load_local_identity().await.unwrap().unwrap();
        let codex_row = store
            .load_local_identity_by_agent_name("codex")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(default_row.peer_id, default.peer_id);
        assert_eq!(codex_row.peer_id, codex.peer_id);
    }

    #[test]
    fn agent_name_is_trimmed_and_must_be_printable() {
        assert_eq!(normalise_agent_name("  codex  ").unwrap(), "codex");
        assert!(matches!(
            normalise_agent_name(" \n "),
            Err(IdentityError::InvalidAgentName(_))
        ));
        assert!(matches!(
            normalise_agent_name("codex\nmain"),
            Err(IdentityError::InvalidAgentName(_))
        ));
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
    async fn migrates_identity_json_when_key_exists_and_row_missing() {
        let home = TempDir::new().unwrap();
        write_owner_only(&LocalIdentity::key_path(home.path()), &[7u8; 32]).unwrap();
        let peer_id = PeerId::new();
        let client_id = ClientId::new();
        std::fs::write(
            home.path().join(LEGACY_IDENTITY_JSON_FILENAME),
            serde_json::json!({
                "version": IDENTITY_STATE_VERSION,
                "peer_id": peer_id,
                "client_id": client_id,
                "created_at_ms": 1234_u64
            })
            .to_string(),
        )
        .unwrap();

        let identity = LocalIdentity::load_or_generate(home.path()).await.unwrap();

        assert_eq!(identity.peer_id, peer_id);
        assert_eq!(identity.client_id, client_id);
        assert_eq!(identity.keypair.secret_bytes(), [7u8; 32]);
        assert!(
            !home.path().join(LEGACY_IDENTITY_JSON_FILENAME).exists(),
            "identity.json is consumed after deterministic ORM migration"
        );
        let store = open_store(home.path()).await.unwrap();
        let stored = store.load_local_identity().await.unwrap().unwrap();
        assert_eq!(stored.peer_id, peer_id);
        assert_eq!(stored.client_id, client_id);
        assert_eq!(stored.created_at_ms, 1234);
    }

    #[tokio::test]
    async fn invalid_identity_json_keeps_partial_state_loud() {
        let home = TempDir::new().unwrap();
        write_owner_only(&LocalIdentity::key_path(home.path()), &[7u8; 32]).unwrap();
        std::fs::write(home.path().join(LEGACY_IDENTITY_JSON_FILENAME), "{").unwrap();

        let result = LocalIdentity::load_or_generate(home.path()).await;

        assert!(matches!(result, Err(IdentityError::LegacyIdentityJson(_))));
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
                agent_name: airc_store::DEFAULT_AGENT_NAME.to_string(),
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
