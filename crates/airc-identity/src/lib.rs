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
/// Her private records, sealed (continuum PRIVACY-OF-THOUGHT.md step 2).
pub mod mind;

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
    /// A READ of a home that already holds identity material (a key file
    /// and/or rows for other agents) found no row for the requested agent.
    /// Identity creation is an explicit act — `airc init` / the mint APIs —
    /// never a side effect of opening a home, so this refuses instead of
    /// minting (card 32f7dd97: a `work board` read once regenerated
    /// identity.key).
    IdentityNotInitialised {
        agent_name: String,
    },
    /// A row for the requested agent exists but her OWN key file is missing.
    /// Under the #402 layout each citizen's secret lives in her own
    /// `identity-<name>.key`; substituting another principal's secret (the
    /// shared legacy file, a neighbour's per-agent file) would load a
    /// different signing identity than the row describes — wire mismatch or
    /// silent re-identification. Refuse and name the missing file instead of
    /// guessing at it.
    KeyMissingForAgent {
        agent_name: String,
    },
    /// An explicit mint (the `generate_and_save*` family) was asked for a
    /// name that already holds an identity row in this home. Minting a twin
    /// beside the existing citizen would fork her history; rekeying is a
    /// named operation, not a side effect of initialisation.
    IdentityAlreadyInitialised {
        agent_name: String,
    },
    /// An explicit mint was asked for a name whose OWN key file already
    /// exists but has no row (orphan material — the residue of an interrupted
    /// or pre-fix mint). Binding a fresh peer_id to unknown secret material
    /// would adopt someone else's identity; recover or deliberately delete
    /// the file first.
    KeyMaterialWithoutState {
        agent_name: String,
    },
    /// An explicit mint inserted its row but could not write the key file:
    /// the home now holds half a citizen (state without secret). Every open
    /// path refuses this half-state fail-closed; restore the key or remove
    /// the orphan row.
    KeyWriteFailedAfterRowInserted {
        agent_name: String,
        source: std::io::Error,
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
            IdentityError::IdentityNotInitialised { agent_name } => write!(
                f,
                "no identity for agent {agent_name:?} in this home — \
                 identity creation is an explicit act (run `airc init` as that agent, or set AIRC_AGENT_NAME to an existing one); a read never mints one"
            ),
            IdentityError::KeyMissingForAgent { agent_name } => write!(
                f,
                "identity row for agent {agent_name:?} exists but its own key file (identity-{agent_name}.key) is missing — \
                 restore that file or remove the orphan row before this home can open as that agent; \
                 the substrate refuses to substitute another principal's secret"
            ),
            IdentityError::IdentityAlreadyInitialised { agent_name } => write!(
                f,
                "agent {agent_name:?} already holds an identity in this home — refusing to mint a twin beside it; \
                 rekeying is an explicit operation, not a side effect of initialisation"
            ),
            IdentityError::KeyMaterialWithoutState { agent_name } => write!(
                f,
                "agent {agent_name:?} has key material on disk (identity-{agent_name}.key) but no identity row — \
                 unknown secret material, possibly the residue of an interrupted mint; recover it or deliberately delete the file before initialising that name"
            ),
            IdentityError::KeyWriteFailedAfterRowInserted { agent_name, source } => write!(
                f,
                "identity row for agent {agent_name:?} was inserted but its key file could not be written ({source}) — \
                 the home now holds half a citizen: restore the missing key or remove the orphan row before opening"
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
            | IdentityError::AgentNameMismatch { .. }
            | IdentityError::IdentityNotInitialised { .. }
            | IdentityError::KeyMissingForAgent { .. }
            | IdentityError::IdentityAlreadyInitialised { .. }
            | IdentityError::KeyMaterialWithoutState { .. } => None,
            IdentityError::KeyWriteFailedAfterRowInserted { source, .. } => Some(source),
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

    /// Path to the secret key belonging to ONE agent identity in `home`.
    ///
    /// A distinct peer identity requires distinct key material (#402). The
    /// wire authenticates by KEY: `PinnedServerVerifier` resolves the
    /// presented pubkey through `PeerKeyRegistry::find_peer`, which maps a
    /// pubkey to exactly ONE peer and rejects anything else as an identity
    /// mismatch. So two agents sharing a keypair are the same principal on
    /// the wire, and minting them separate `peer_id`s is a claim the
    /// transport cannot uphold — the peer store ends up with N rows for one
    /// key and sends land on whichever twin routing picked (measured: 73
    /// rows / 59 pubkeys, only 4 rows carrying any endpoint).
    ///
    /// The DEFAULT agent keeps the historical `identity.key` filename, so
    /// every existing home loads exactly as before.
    pub fn agent_key_path(home: &Path, agent_name: &str) -> PathBuf {
        if agent_name == airc_store::DEFAULT_AGENT_NAME {
            Self::key_path(home)
        } else {
            home.join(format!("identity-{agent_name}.key"))
        }
    }

    /// Load the existing identity from `home`, or — if no identity material
    /// exists at all (no key file, no legacy json, no rows) — mint one. See
    /// [`Self::load_or_generate_as_with_peer_id`] for which situations load and
    /// which refuse; a read never mints over existing state (card 32f7dd97).
    pub async fn load_or_generate(home: &Path) -> Result<Self, IdentityError> {
        let agent_name = requested_agent_name(None)?;
        Self::load_or_generate_as(home, agent_name).await
    }

    /// Load an existing identity for an explicit agent name, or mint one if
    /// `home` holds no identity material at all. Programmatic partner of
    /// `airc init --as <name>`; see [`Self::load_or_generate_as_with_peer_id`]
    /// for which situations load and which refuse.
    pub async fn load_or_generate_as(
        home: &Path,
        agent_name: impl AsRef<str>,
    ) -> Result<Self, IdentityError> {
        Self::load_or_generate_as_with_peer_id(home, agent_name, None).await
    }

    /// True if `home` holds any per-agent key file (`identity-<name>.key`,
    /// #402 layout). The shared `identity.key` is deliberately not counted:
    /// callers ask this only after finding the shared file absent, and a live
    /// citizen's secret half lives in her own file when she has one — that is
    /// exactly what makes "rows exist but no key for THIS agent" an unopened
    /// identity rather than lost material.
    fn any_per_agent_key(home: &Path) -> bool {
        std::fs::read_dir(home)
            .map(|entries| {
                entries.flatten().any(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with("identity-") && name.ends_with(".key")
                })
            })
            .unwrap_or(false)
    }

    /// Load an existing identity for `agent_name`, or — on a home that holds
    /// NO identity material at all (no key file, no legacy json, no rows)
    /// mint one, adopting the supplied `peer_id` if any.
    ///
    /// This is the OPEN path: it must never invent an identity for a home
    /// that already has some — that would be a read minting over existing
    /// state. The arms below encode exactly which situations are loadable and
    /// which must fail, naming what is missing:
    ///
    /// - key + row both present → load.
    /// - legacy `identity.json` whose agent matches → migrate to the modern
    ///   layout (a one-way upgrade of THIS identity, not a mint).
    /// - key present, no row for this agent, rows exist for other agents →
    ///   [`IdentityError::IdentityNotInitialised`]: opening must never add a
    ///   citizen beside existing ones; adding an agent to a populated home
    ///   needs its own explicit verb (the shared-key mint refuses there).
    /// - legacy `identity.json` describes another agent →
    ///   [`IdentityError::AgentNameMismatch`]: the stored material belongs to
    ///   someone else; refuse rather than fall back to it.
    /// - key present, no row for any agent → [`IdentityError::PartialState`].
    /// - legacy `identity.json` present, no key file →
    ///   [`IdentityError::PartialState`]: state exists in its pre-ORM form
    ///   but the secret half is gone; minting here would rotate an existing
    ///   citizen.
    /// - rows exist under other agents' names, no shared key file, and at
    ///   least one per-agent key survives → [`IdentityError::IdentityNotInitialised`]:
    ///   the home holds living citizens; opening must not add a citizen beside them,
    ///   so the requested identity is simply not initialised here.
    /// - rows present (any agent), no key material at all →
    ///   [`IdentityError::PartialState`]:
    ///   the secret half is gone fleet-wide; minting here would rotate an existing
    ///   citizen.
    /// - nothing at all → fresh mint (the ONLY arm that writes).
    pub async fn load_or_generate_as_with_peer_id(
        home: &Path,
        agent_name: impl AsRef<str>,
        peer_id: Option<PeerId>,
    ) -> Result<Self, IdentityError> {
        let agent_name = normalise_agent_name(agent_name.as_ref())?;
        let store = open_store(home).await?;
        let key_exists = Self::key_path(home).exists();
        let stored = store.load_local_identity_by_agent_name(&agent_name).await?;
        match (key_exists, stored) {
            (true, Some(stored)) => Self::load_with_metadata_for_agent(home, stored, &agent_name),
            // Key present but no row for the requested agent. A legacy
            // `identity.json` may still carry this identity; migrating it is a
            // load of existing material, not creation.
            (true, None) => match migrate_legacy_identity_json(home, &store).await? {
                Some(stored) if stored.agent_name == agent_name => {
                    Self::load_with_metadata(home, stored)
                }
                Some(_) => Err(IdentityError::AgentNameMismatch {
                    requested: agent_name.to_string(),
                    found: airc_store::DEFAULT_AGENT_NAME.to_string(),
                }),
                None if store.has_local_identity_rows().await? => {
                    // The home holds other agents' rows but no row for this
                    // name. Refuse rather than mint a fresh twin beside them:
                    // opening must never add citizens.
                    Err(IdentityError::IdentityNotInitialised {
                        agent_name: agent_name.to_string(),
                    })
                }
                None => Err(IdentityError::PartialState {
                    key_exists: true,
                    state_exists: false,
                }),
            },
            // The ONLY fresh-mint arm: no shared key file, no row for this
            // agent.
            (false, None) => {
                if Self::any_per_agent_key(home) {
                    // Per-agent key material survives with no shared key file.
                    // With rows present it belongs to living citizens — opening
                    // as THIS name must not add a citizen beside them; with no
                    // rows at all it is orphaned secret material (the residue of
                    // an interrupted or pre-fix mint) whose owner's state half is
                    // unknown. Either way this home holds surviving identity
                    // material, and the ONLY fresh-mint arm must not write over
                    // it: refuse and name both halves.
                    if store.has_local_identity_rows().await? {
                        return Err(IdentityError::IdentityNotInitialised {
                            agent_name: agent_name.to_string(),
                        });
                    }
                    return Err(IdentityError::PartialState {
                        key_exists: true,
                        state_exists: false,
                    });
                }
                if store.has_local_identity_rows().await? {
                    // Rows exist but NO key material at all: the secret half
                    // is gone fleet-wide. Minting a fresh identity here would
                    // rotate an existing citizen's home — refuse and name both
                    // halves, as the doc above promises.
                    return Err(IdentityError::PartialState {
                        key_exists: false,
                        state_exists: true,
                    });
                }
                if home.join(LEGACY_IDENTITY_JSON_FILENAME).exists() {
                    // Legacy state without a key file: minting here would
                    // rotate an existing citizen. Refuse and name both halves.
                    return Err(IdentityError::PartialState {
                        key_exists: false,
                        state_exists: true,
                    });
                }
                match peer_id {
                    Some(peer_id) => {
                        Self::generate_and_save_as_with_peer_id(home, &store, &agent_name, peer_id)
                            .await
                    }
                    None => Self::generate_and_save_as(home, &store, &agent_name).await,
                }
            }
            // A row exists for THIS agent but no shared key file: under the
            // #402 layout a named citizen's secret lives in its own per-agent
            // key file, so if that one is present we can load her exactly as
            // before — this arm must not report "key missing" when her key is
            // simply elsewhere. If neither her own file nor the shared one
            // exists, the secret half is genuinely gone: refuse and name both
            // halves (minting here would rotate an existing citizen).
            (false, Some(stored)) => {
                if Self::agent_key_path(home, &agent_name).exists() {
                    Self::load_with_metadata_for_agent(home, stored, &agent_name)
                } else {
                    Err(IdentityError::PartialState {
                        key_exists: false,
                        state_exists: true,
                    })
                }
            }
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
        // Under the #402 layout each citizen's secret lives in her OWN key file
        // (for the default agent that is the shared `identity.key` itself). A row
        // whose own key file is missing must refuse to open: substituting another
        // principal's secret — the shared legacy file, a neighbour's per-agent
        // file — would load a different signing identity than this row describes.
        let key_path = Self::agent_key_path(home, &stored.agent_name);
        if !key_path.exists() {
            return Err(IdentityError::KeyMissingForAgent {
                agent_name: stored.agent_name.clone(),
            });
        }
        let key_bytes = std::fs::read(&key_path)?;
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

    /// Generate a fresh identity + persist it for the default agent. The
    /// row is inserted FIRST and the secret key file (0600) second, so any
    /// failure before the write leaves no material on disk; if the key
    /// write fails after the insert, the residue is a row without its
    /// secret — every open path refuses that half-state fail-closed
    /// (`KeyMissingForAgent` / `PartialState`).
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
        // The peer_id is airc's to assign, so the default mint stamps a
        // fresh random one. Hosts that derive an actor's identifier BEFORE
        // attach (e.g. Continuum personas, whose name + home are projected
        // from a `persona_id`) use `generate_and_save_as_with_peer_id` to make
        // the two cohere.
        Self::generate_and_save_as_with_peer_id(home, store, agent_name, PeerId::new()).await
    }

    /// Like [`generate_and_save_as`], but ADOPTS a caller-supplied `peer_id`
    /// instead of minting a random one.
    ///
    /// The `peer_id` is a stable uuid airc assigns and persists — it is NOT a
    /// hash of the keypair (the keypair is separate signing material, still
    /// freshly generated here). So there is no cryptographic reason it must be
    /// minted inside airc: an embedding host that has already derived an
    /// identifier can hand it in, and `peer_id()` will equal it.
    ///
    /// The motivating consumer is Continuum's per-persona runtime: a persona's
    /// `agent_name` (and thus her home directory) is projected from a
    /// `persona_id` chosen before `attach_as` runs. Passing that `persona_id`
    /// here as the `peer_id` makes `airc.peer_id() == persona_id == the seed her
    /// name was derived from` — coherent identity from birth, with no
    /// throwaway-uuid divergence to collapse after the fact.
    ///
    /// This is the FRESH-mint path only. A resumed identity (key + row already
    /// on disk) loads its stored `peer_id`; a supplied one is never consulted,
    /// so an existing citizen's identity is never rotated.
    pub async fn generate_and_save_as_with_peer_id(
        home: &Path,
        store: &SqliteEventStore,
        agent_name: impl AsRef<str>,
        peer_id: PeerId,
    ) -> Result<Self, IdentityError> {
        let agent_name = normalise_agent_name(agent_name.as_ref())?;
        ensure_home_dir(home)?;

        // Custody pre-checks BEFORE any write: an explicit mint must never
        // leave half a citizen behind. A row for this name means the identity
        // already exists — rekeying is a named operation, not a side effect of
        // this verb (and a blind insert would fail on the unique index while
        // key material was already on disk). Key material without a row is
        // orphaned secret whose owner's state half is unknown; writing beside
        // it would leave two secrets for one name. Refuse both, before touching
        // anything (card 32f7dd97: a failed mint must preserve custody).
        if store
            .load_local_identity_by_agent_name(&agent_name)
            .await?
            .is_some()
        {
            return Err(IdentityError::IdentityAlreadyInitialised {
                agent_name: agent_name.to_string(),
            });
        }
        let key_path = Self::agent_key_path(home, &agent_name);
        if key_path.exists() {
            return Err(IdentityError::KeyMaterialWithoutState {
                agent_name: agent_name.to_string(),
            });
        }

        let keypair = PeerKeypair::generate();
        let client_id = ClientId::new();
        let created_at_ms = now_ms()?;

        // Row FIRST, then key — the reverse order is the custody bug this card
        // fixes. Writing the named key before the row insert meant a duplicate
        // (a pre-existing legacy row for the same name) failed the insert while
        // leaving an orphan `identity-<name>.key` behind that shadows whatever
        // secret that row actually uses: the next open would load a different
        // signing identity than the stored peer_id. With the row first, any
        // failure before the key write leaves no new material on disk at all;
        // and if the key write fails after the insert succeeds, the residue is
        // a row without its secret — which every open path refuses with
        // KeyMissingForAgent/PartialState (fail-closed) rather than loading a
        // different principal.
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

        // Each agent's secret lives in its OWN key file (#402): a distinct
        // peer identity needs distinct key material, and writing every named
        // agent to the shared `identity.key` would make two citizens of one
        // home indistinguishable on the wire. The DEFAULT agent keeps the
        // historical shared filename (agent_key_path resolves it); any other
        // name gets `identity-<name>.key`. create_new still refuses to
        // overwrite whatever file already exists at that path — rekeying is a
        // named operation, not a side effect of this mint.
        write_owner_only(&key_path, &keypair.secret_bytes()).map_err(|e| {
            // The row now exists without its secret half; say so explicitly so
            // the operator knows exactly what to repair (restore or remove the
            // orphan row) — a bare io error would hide that.
            IdentityError::KeyWriteFailedAfterRowInserted {
                agent_name: agent_name.to_string(),
                source: e,
            }
        })?;

        Ok(Self {
            keypair,
            peer_id,
            client_id,
            agent_name,
        })
    }
    /// Explicit initialisation verb behind `airc init` (no `--as`): the
    /// default agent's [`Self::initialize_as`]. The legacy `identity.json` /
    /// shared `identity.key` layout belongs to the default citizen, so her
    /// explicit init runs through exactly the same custody-safe path — no
    /// special-casing of the anonymous scope.
    pub async fn initialize(home: &Path, store: &SqliteEventStore) -> Result<Self, IdentityError> {
        Self::initialize_as(home, store, airc_store::DEFAULT_AGENT_NAME).await
    }

    /// Explicit initialisation verb behind `airc init [--as <name>]`: load an
    /// existing identity for `agent_name` (init is idempotent), or — only when
    /// nothing of this agent's survives in the home — mint one through the
    /// custody-safe [`Self::generate_and_save_as`] path.
    ///
    /// Unlike the OPEN path (`load_or_generate*`), which must never add a
    /// citizen beside existing ones, an explicit init MAY add a named citizen
    /// to a populated home — that is its whole purpose under the #402 layout.
    /// It still refuses every half-state fail-closed:
    ///
    /// - a row for this name exists → load it (half-states surface as
    ///   `KeyMissingForAgent` / `PartialState`; rekeying is never a side
    ///   effect of init).
    /// - no row, but the legacy `identity.json` survives WITH its key file →
    ///   migrate that identity (a load of existing material, not creation);
    ///   the json always describes the default agent, so initialising any
    ///   other name here is an [`IdentityError::AgentNameMismatch`].
    /// - no row, legacy `identity.json` WITHOUT its key file →
    ///   [`IdentityError::PartialState`]: the secret half of a pre-ORM citizen
    ///   is gone; minting over it would rotate them.
    /// - nothing of this agent's survives → fresh custody-safe mint (row first,
    ///   per-agent key second), which may sit beside other citizens' material —
    ///   explicit intent to add a citizen is exactly what init exists for.
    pub async fn initialize_as(
        home: &Path,
        store: &SqliteEventStore,
        agent_name: impl AsRef<str>,
    ) -> Result<Self, IdentityError> {
        let agent_name = normalise_agent_name(agent_name.as_ref())?;
        ensure_home_dir(home)?;

        // Idempotent resume: a row for this name means the identity already
        // exists here. Load it — half-states fail closed (KeyMissingForAgent /
        // PartialState) instead of being minted over, and rekeying is never a
        // side effect of initialisation.
        if let Some(stored) = store.load_local_identity_by_agent_name(&agent_name).await? {
            return Self::load_with_metadata_for_agent(home, stored, &agent_name);
        }

        // No row for this name. A surviving legacy `identity.json` is the state
        // half of a pre-ORM identity (always the default agent's): init must
        // resume what exists rather than create over it.
        if home.join(LEGACY_IDENTITY_JSON_FILENAME).exists() {
            if !Self::key_path(home).exists() {
                // State half without its secret: minting here would rotate a
                // pre-ORM citizen whose key is gone. Refuse and name both halves.
                return Err(IdentityError::PartialState {
                    key_exists: false,
                    state_exists: true,
                });
            }
            return match migrate_legacy_identity_json(home, store).await? {
                Some(stored) if stored.agent_name == agent_name => {
                    Self::load_with_metadata(home, stored)
                }
                Some(_) => Err(IdentityError::AgentNameMismatch {
                    requested: agent_name.to_string(),
                    found: airc_store::DEFAULT_AGENT_NAME.to_string(),
                }),
                None => unreachable!("legacy identity.json existence checked above"),
            };
        }

        // Nothing of this agent's survives in the home: explicit mint with
        // custody-safe ordering. This is the one path that adds a citizen
        // beside existing ones — and only because an operator asked for it.
        Self::generate_and_save_as(home, store, agent_name).await
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

/// Resolve the active identity name without loading or creating identity state.
pub fn requested_agent_name(explicit: Option<&str>) -> Result<String, IdentityError> {
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
    // R4 (card 32f7dd97): every key-material write is probed loudly with
    // the path, the previous fingerprint, and the new fingerprint — a key
    // change no one can see is equivalent to a citizen silently losing
    // their identity. `create_new` below means "previous" can only be
    // non-absent on the refusal path, where we still name it: if this
    // function ever gains an overwrite mode, the probe already records
    // both halves.
    let new_fingerprint = sha256_hex(bytes);
    let previous_fingerprint = match std::fs::read(path) {
        Ok(existing) => Some(sha256_hex(&existing)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    // Identity creation must never replace existing key material. In a legacy
    // named-agent home, identity.key still belongs to that agent: adding the
    // default identity must refuse rather than silently rotate the original.
    // create_new also closes the check-then-write race between concurrent mints.
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            eprintln!(
                "[airc-identity] KEY WRITE REFUSED: {} (previous fingerprint {}, refusing to overwrite)",
                path.display(),
                previous_fingerprint.as_deref().unwrap_or("unreadable")
            );
            std::io::Error::new(error.kind(), format!(
                "refusing to overwrite existing identity key {}; select the existing named agent or use a separate home",
                path.display()
            ))
        } else {
            error
        }
    })?;
    file.write_all(bytes)?;
    eprintln!(
        "[airc-identity] KEY WRITE: {} (previous fingerprint {}, new fingerprint {})",
        path.display(),
        previous_fingerprint.as_deref().unwrap_or("absent"),
        new_fingerprint
    );
    Ok(())
}

/// sha256 of `bytes` as lowercase hex — the fingerprint format for the
/// key-write probes (R4).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
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

        // A default CLI read of a named home used to MINT OVER its key; the
        // running citizen survived until restart, then failed peer trust.
        // Now the open refuses outright and names what it found (card 32f7dd97).
        let error = match LocalIdentity::load_or_generate_as(home.path(), "default").await {
            Err(error) => error,
            Ok(_) => panic!("the existing key must not be overwritten"),
        };
        assert!(matches!(
            error,
            IdentityError::IdentityNotInitialised { ref agent_name }
                if agent_name == airc_store::DEFAULT_AGENT_NAME
        ));
        assert!(store.load_local_identity().await.unwrap().is_none());
        let resumed = LocalIdentity::load_or_generate_as(home.path(), "codex")
            .await
            .unwrap();
        assert_eq!(resumed.peer_id, identity.peer_id);
        assert_eq!(
            resumed.keypair.public_bytes(),
            identity.keypair.public_bytes()
        );
    }

    #[tokio::test]
    async fn open_refuses_to_add_a_second_agent_to_an_existing_scope() {
        // what this catches: #402's layout used to be created by OPENING — a
        // read that found no row for its name silently minted a second
        // identity into the home; and in the card 32f7dd97 incident a default
        // open minted OVER the named citizen's key. The contract is now
        // inverted: opening never adds citizens to a populated home — it fails,
        // naming what is missing for the agent asked for.
        let home = TempDir::new().unwrap();
        let default = LocalIdentity::load_or_generate(home.path()).await.unwrap();

        let error = match LocalIdentity::load_or_generate_as(home.path(), "codex").await {
            Err(error) => error,
            Ok(_) => panic!("opening must not mint a second agent into the home"),
        };
        assert!(matches!(
            error,
            IdentityError::IdentityNotInitialised { ref agent_name }
                if agent_name == "codex"
        ));

        // And the first citizen is untouched: same keypair on reload.
        let resumed = LocalIdentity::load_or_generate(home.path()).await.unwrap();
        assert_eq!(resumed.peer_id, default.peer_id);
        assert_eq!(
            resumed.keypair.secret_bytes(),
            default.keypair.secret_bytes()
        );

        // No codex row was created by the refused open.
        let store = open_store(home.path()).await.unwrap();
        assert!(store
            .load_local_identity_by_agent_name("codex")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn fresh_mint_adopts_a_supplied_peer_id_but_resume_ignores_it() {
        // what this catches: the coherence contract Continuum relies on — a
        // fresh mint stamps the caller's peer_id (so name/home/peer_id cohere
        // from birth), while a resume keeps the STORED identity even if a
        // different peer_id is supplied (an existing citizen is never rotated).
        let home = TempDir::new().unwrap();
        let chosen = PeerId::new();

        let first =
            LocalIdentity::load_or_generate_as_with_peer_id(home.path(), "helper", Some(chosen))
                .await
                .unwrap();
        assert_eq!(
            first.peer_id, chosen,
            "fresh mint must adopt the supplied peer_id"
        );

        // Resume with a DIFFERENT supplied id — the stored one wins.
        let other = PeerId::new();
        assert_ne!(other, chosen);
        let second =
            LocalIdentity::load_or_generate_as_with_peer_id(home.path(), "helper", Some(other))
                .await
                .unwrap();
        assert_eq!(
            second.peer_id, chosen,
            "resume must ignore a supplied peer_id"
        );
        assert_eq!(
            first.keypair.secret_bytes(),
            second.keypair.secret_bytes(),
            "resume keeps the same keypair"
        );

        // And the None path still mints a random id (historical behavior).
        let home2 = TempDir::new().unwrap();
        let random = LocalIdentity::load_or_generate_as_with_peer_id(home2.path(), "helper", None)
            .await
            .unwrap();
        assert_ne!(random.peer_id, chosen);
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

    // what this catches: two agent identities in ONE home sharing a keypair
    // (#402). The wire authenticates by KEY — PinnedServerVerifier resolves a
    // presented pubkey via PeerKeyRegistry::find_peer, which maps a pubkey to
    // exactly ONE peer and rejects anything else as an identity mismatch. So a
    // shared key makes distinct peer_ids unrepresentable downstream: peers
    // enrol a row per peer_id all carrying the same pubkey, and sends land on
    // whichever twin routing picked (measured on the live store: 73 rows / 59
    // pubkeys, only 4 rows with any endpoint). Goes RED the moment a new agent
    // identity borrows the home's existing key again.
    #[tokio::test]
    async fn two_agents_in_one_home_are_cryptographically_distinct() {
        let home = TempDir::new().unwrap();

        // The first agent opens a FRESH home — the only arm where the open
        // path mints. Adding a second citizen is an explicit act (R1): the
        // open path must refuse, and `generate_and_save_as` does the minting.
        let store = open_store(home.path()).await.unwrap();
        let first = LocalIdentity::load_or_generate_as(home.path(), "alpha")
            .await
            .unwrap();
        assert!(matches!(
            LocalIdentity::load_or_generate_as(home.path(), "beta").await,
            Err(IdentityError::IdentityNotInitialised { .. })
        ));
        let second = LocalIdentity::generate_and_save_as(home.path(), &store, "beta")
            .await
            .unwrap();

        assert_ne!(
            first.peer_id, second.peer_id,
            "distinct agents must have distinct peer_ids"
        );
        assert_ne!(
            first.keypair.public_bytes(),
            second.keypair.public_bytes(),
            "distinct peer_ids REQUIRE distinct key material — a shared pubkey \
             makes find_peer ambiguous and misroutes every send (#402)"
        );

        // …and each is stable across reload: the second agent must come back
        // with its OWN key, not the home's shared one.
        let reloaded = LocalIdentity::load_or_generate_as(home.path(), "beta")
            .await
            .unwrap();
        assert_eq!(reloaded.peer_id, second.peer_id);
        assert_eq!(
            reloaded.keypair.public_bytes(),
            second.keypair.public_bytes(),
            "an existing agent must never rotate its key on reload"
        );
    }

    // what this catches (P1 #1, card 32f7dd97): a failed mint must preserve
    // custody. The pre-fix ordering wrote the named key file FIRST and then
    // inserted the row; against a legacy home whose named citizen's secret
    // lives in the shared identity.key, the duplicate insert would fail while
    // leaving an orphan identity-<name>.key behind that shadows the old
    // secret — the next open loads a different signing principal than the
    // stored peer_id. Now the custody pre-check runs before any write: an
    // existing row for this name refuses, and no material is left on disk.
    #[tokio::test]
    async fn explicit_mint_refuses_a_legacy_named_row_using_the_shared_key() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path()).unwrap();
        // Legacy layout: the named citizen's secret lives in the shared
        // identity.key (pre-#402), with a row already present for that name.
        std::fs::write(LocalIdentity::key_path(home.path()), [0u8; 32]).unwrap();
        let store = open_store(home.path()).await.unwrap();
        store
            .insert_local_identity(StoredLocalIdentity {
                peer_id: PeerId::new(),
                client_id: ClientId::new(),
                version: IDENTITY_STATE_VERSION,
                created_at_ms: 1,
                identity: Identity::default(),
                agent_name: "codex".to_string(),
            })
            .await
            .unwrap();

        let result = LocalIdentity::generate_and_save_as(home.path(), &store, "codex").await;
        assert!(matches!(
            result,
            Err(IdentityError::IdentityAlreadyInitialised {
                ref agent_name
            }) if agent_name == "codex"
        ));
        // Custody preserved: no new key material on disk — the old secret (the
        // shared file) remains the only one, unshadowed.
        assert!(!LocalIdentity::agent_key_path(home.path(), "codex").exists());
    }

    // what this catches (P1 #1): key material without a row is orphaned secret
    // whose owner's state half is unknown — binding a fresh peer_id to it would
    // adopt someone else's identity. The mint must refuse BEFORE touching
    // anything and leave the orphan exactly as found: no new row, file bytes
    // unchanged (recovery or deletion is an operator's explicit act).
    #[tokio::test]
    async fn explicit_mint_refuses_orphan_key_material() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path()).unwrap();
        let key_path = LocalIdentity::agent_key_path(home.path(), "codex");
        std::fs::write(&key_path, [7u8; 32]).unwrap();
        let bytes_before = std::fs::read(&key_path).unwrap();

        let store = open_store(home.path()).await.unwrap();
        let result = LocalIdentity::generate_and_save_as(home.path(), &store, "codex").await;
        assert!(matches!(
            result,
            Err(IdentityError::KeyMaterialWithoutState {
                ref agent_name
            }) if agent_name == "codex"
        ));
        // No row was created — the orphan remains exactly as found.
        assert!(store
            .load_local_identity_by_agent_name("codex")
            .await
            .unwrap()
            .is_none());
        assert_eq!(bytes_before, std::fs::read(&key_path).unwrap());
    }

    // what this catches (P1 #2): existing_agent_key_path used to fall back to
    // the shared identity.key when a named agent's own key was missing — after
    // default+named and deleting the named key, open returned the NAMED
    // peer_id signed with the DEFAULT secret: a different signing principal
    // under one citizen's name. Now load_with_metadata reads that agent's OWN
    // file directly; missing means refuse, never substitute.
    #[tokio::test]
    async fn missing_own_key_refuses_without_substituting_the_shared_secret() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path()).unwrap();

        // Two citizens in one home: the default (shared identity.key) and a
        // named agent with her own key file.
        let first = LocalIdentity::load_or_generate_as(home.path(), airc_store::DEFAULT_AGENT_NAME)
            .await
            .unwrap();
        let store = open_store(home.path()).await.unwrap();
        let second = LocalIdentity::generate_and_save_as(home.path(), &store, "codex")
            .await
            .unwrap();

        // The named citizen's secret half is lost (deleted, moved, corrupted
        // away). Her row survives.
        std::fs::remove_file(LocalIdentity::agent_key_path(home.path(), "codex")).unwrap();

        // Opening as codex must refuse — with the shared identity.key sitting
        // right there on disk. Loading her peer_id under someone else's secret
        // is exactly the mis-identification this card kills.
        let result = LocalIdentity::load_or_generate_as(home.path(), "codex").await;
        assert!(matches!(
            result,
            Err(IdentityError::KeyMissingForAgent {
                ref agent_name
            }) if agent_name == "codex"
        ));

        // The default citizen is untouched: same peer_id, same key material.
        let reloaded_default =
            LocalIdentity::load_or_generate_as(home.path(), airc_store::DEFAULT_AGENT_NAME)
                .await
                .unwrap();
        assert_eq!(reloaded_default.peer_id, first.peer_id);
        assert_eq!(
            reloaded_default.keypair.public_bytes(),
            first.keypair.public_bytes()
        );

        // And codex's row was never rewritten to point at the default's secret.
        let stored_codex = store
            .load_local_identity_by_agent_name("codex")
            .await
            .unwrap()
            .expect("codex's row must survive a failed open");
        assert_eq!(stored_codex.peer_id, second.peer_id);
    }

    // what this catches (P2 #4): the per-agent material guard used to run only
    // when rows existed — an orphan identity-<name>.key with NO row and NO
    // shared key still let the default read-mint arm write a fresh
    // identity.key beside surviving secret material. Any surviving identity
    // material must block the ONLY fresh-mint arm: this home holds unknown
    // secrets, and minting over them would rotate someone's citizen.
    #[tokio::test]
    async fn orphan_per_agent_key_blocks_the_default_read_mint() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path()).unwrap();
        // Orphan per-agent key, no rows, no shared identity.key — the residue
        // of an interrupted or pre-fix mint.
        std::fs::write(
            LocalIdentity::agent_key_path(home.path(), "codex"),
            [9u8; 32],
        )
        .unwrap();

        let result =
            LocalIdentity::load_or_generate_as(home.path(), airc_store::DEFAULT_AGENT_NAME).await;
        assert!(matches!(
            result,
            Err(IdentityError::PartialState {
                key_exists: true,
                state_exists: false,
            })
        ));

        // The mint arm must have written NOTHING: no shared key, no rows.
        assert!(!LocalIdentity::key_path(home.path()).exists());
        let store = open_store(home.path()).await.unwrap();
        assert!(!store.has_local_identity_rows().await.unwrap());
    }
}
