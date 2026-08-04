//! Mesh identity resolution — who is "us" on this machine?
//!
//! Resolves the user's authenticated Git/GitHub identity once per
//! machine and caches it for the [`crate::subscriptions::SubscriptionSet`]
//! RoomId derivation. Without this, every scope on every user's
//! machine falls back to [`crate::subscriptions::MeshIdentity::unset`],
//! which is fine for tests but collides every user's `#general` onto
//! the same `RoomId` — a privacy bug.
//!
//! ## THE OWNER'S GITHUB LOGIN IS THE KEY. There is no substitute.
//!
//! This identity is the **human owner's** — one login across every
//! machine they own. It is NOT a persona identity and NOT an agent
//! identity: those are separate identities that live *inside* the
//! owner's grid (peer ids, persona ids) and never namespace a room.
//!
//! A machine cannot know who its human is by looking at itself. So the
//! resolution order is short, and it does not end in a guess:
//!
//! 1. **`AIRC_MESH_IDENTITY`** — the owner stating their own login
//!    explicitly (offline boxes, CI, containers). Persisted as
//!    `operator`; sticky forever.
//! 2. **Cached `gh_api_user`**, if fresh (within [`DEFAULT_TTL_MS`]).
//! 3. **`gh api user --jq .login`**.
//! 4. **A stale cached `gh_api_user`** when gh is unreachable — a stale
//!    copy of the key is still the key; the owner's login does not
//!    change because the network did.
//! 5. Otherwise **[`MeshIdentityError::Unresolved`]**. Loud, no key, no
//!    rooms.
//!
//! ## Why there is no fallback
//!
//! Room UUIDs are `UUIDv5(mesh_identity ‖ NUL ‖ channel_name)`. Any
//! value that is not the owner's login therefore mints a PRIVATE
//! namespace that only this machine can see: it publishes beacons the
//! account never reads, reads a room nobody writes, and reports a
//! healthy join throughout. That is how a node forms its own island.
//!
//! The old chain fell back to `git config user.email`, then to a
//! fabricated `local:<host>:<user>`. Both are machine facts, not owner
//! facts, and both did exactly that — bigmama's `#general` derived under
//! `local:unknown-host:unknown-user` while the M5 derived under the gh
//! login, so every frame between them died as `unknown_channel` with no
//! error anywhere. Guessing the owner's identity is never better than
//! saying "I don't know who my owner is yet".
//!
//! ## Caching
//!
//! Persisted to the `mesh_identity` ORM table. Re-resolution kicks in
//! after `DEFAULT_TTL_MS`; cache hits never
//! shell out, so ten local scopes opening at once produce at most
//! one `gh` call.
//!
//! ## Test injection
//!
//! [`resolve_with`] takes a closure that produces the raw identity
//! string, sidestepping the shell-out. Production code calls
//! [`resolve`] which uses the gh+git fallback resolver. Tests pass
//! a fixed-string closure.

use std::path::PathBuf;
use std::process::Command;

use airc_store::{EventStore, StoredMeshIdentity};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::subscriptions::MeshIdentity;

const MESH_IDENTITY_SCOPE: &str = "default";
/// Default cache TTL: 24h. Re-resolution after this re-checks gh /
/// git in case the operator switched accounts.
pub const DEFAULT_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// Where the resolved identity came from. Closed set so callers (CLI
/// status output, doctor) can pattern-match exhaustively when
/// explaining the cache state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `gh api user --jq .login` succeeded. Canonical.
    GhApiUser,
    /// `git config user.email` was used because `gh` was unavailable
    /// or unauthenticated. Acceptable but won't converge with other
    /// machines that resolved via `gh`.
    GitEmail,
    /// Neither succeeded; identity is a deterministic but
    /// machine-local fallback. Cross-machine convergence is broken
    /// in this state — surface it loudly.
    LocalHostUser,
    /// Operator-supplied via env or CLI override. Trusted as-is.
    Operator,
}

/// Persisted cache shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedIdentity {
    pub version: u32,
    pub identity: String,
    pub source: Source,
    pub resolved_at_ms: u64,
    pub ttl_ms: u64,
}

impl CachedIdentity {
    /// True if `now_ms` is past `resolved_at_ms + ttl_ms`.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.resolved_at_ms) >= self.ttl_ms
    }

    /// Convert to the typed `MeshIdentity` used by RoomId derivation.
    pub fn as_mesh_identity(&self) -> MeshIdentity {
        MeshIdentity::new(self.identity.clone())
    }
}

/// What can go wrong resolving/persisting the identity.
#[derive(Debug)]
pub enum MeshIdentityError {
    Store(airc_store::StoreError),
    Clock(std::time::SystemTimeError),
    UnknownSource(String),
    /// No owner identity, and none may be invented. `gh` cannot answer and
    /// nothing usable is cached. Deriving rooms from a machine-local guess
    /// would put this node on a private mesh only it can see, so the caller
    /// gets an error instead of a silent island.
    Unresolved,
}

impl std::fmt::Display for MeshIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "mesh identity store: {error}"),
            Self::Clock(error) => write!(f, "mesh identity clock: {error}"),
            Self::UnknownSource(source) => write!(f, "unknown mesh identity source: {source}"),
            Self::Unresolved => write!(
                f,
                "no owner identity: `gh api user` could not answer and no gh login is \
                 cached. airc will not invent one — a guessed identity derives room UUIDs \
                 only this machine can see. Fix with `gh auth login`, or state the owner \
                 login explicitly via AIRC_MESH_IDENTITY=<github-login>"
            ),
        }
    }
}

impl std::error::Error for MeshIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::UnknownSource(_) | Self::Unresolved => None,
        }
    }
}

impl From<airc_store::StoreError> for MeshIdentityError {
    fn from(value: airc_store::StoreError) -> Self {
        Self::Store(value)
    }
}

impl From<std::time::SystemTimeError> for MeshIdentityError {
    fn from(value: std::time::SystemTimeError) -> Self {
        Self::Clock(value)
    }
}

/// Resolve the owner's identity (`AIRC_MESH_IDENTITY` → `gh api user`)
/// and persist. Most callers want this. Errors rather than inventing an
/// identity — see the module docs.
pub async fn resolve(store: &dyn EventStore) -> Result<CachedIdentity, MeshIdentityError> {
    resolve_with(store, default_resolver, now_ms()?).await
}

/// Resolve with an injected resolver closure. The closure returns
/// `Some((identity, source))` when it can name the OWNER, `None` when it
/// cannot — and `None` is terminal unless a usable gh login is cached.
///
/// Used by tests to bypass the `gh` shell-out, and by production callers
/// via [`resolve`].
pub async fn resolve_with<F>(
    store: &dyn EventStore,
    resolver: F,
    now_ms: u64,
) -> Result<CachedIdentity, MeshIdentityError>
where
    F: FnOnce() -> Option<(String, Source)>,
{
    if let Some(cached) = load_cached(store).await? {
        // The owner stated their own login (`AIRC_MESH_IDENTITY`, test seed).
        // Trusted as-is, never expires, never re-probed — treating it as
        // TTL-bounded would force the gh shell-out the operator was avoiding
        // (Windows CI runners hung on it when a tiny seeded `resolved_at_ms`
        // made is_expired return true every call).
        if cached.source == Source::Operator {
            return Ok(cached);
        }
        // A fresh gh login is the key. Use it without re-probing.
        if cached.source == Source::GhApiUser && !cached.is_expired(now_ms) {
            return Ok(cached);
        }
        match resolver() {
            Some((identity, source)) => {
                let entry = persisted_entry(identity, source, now_ms);
                save(store, &entry).await?;
                Ok(entry)
            }
            None if cached.source == Source::GhApiUser => {
                // Expired and gh is unreachable right now. A STALE COPY OF THE
                // KEY IS STILL THE KEY — the owner's login did not change
                // because the network did. Erroring here would take a
                // converged machine off its own mesh over a transient outage.
                Ok(cached)
            }
            None => {
                // A legacy `git_email` / `local_host_user` row, minted before
                // this machine could authenticate. That is a machine fact, not
                // the owner's login: every room UUID derived from it belongs to
                // a namespace only this machine can see. Refuse it.
                Err(MeshIdentityError::Unresolved)
            }
        }
    } else {
        // No cache. The key comes from the owner or from gh — never from this
        // box's hostname. A machine cannot know who its human is by looking at
        // itself, and a guess is what mints the island.
        match resolver() {
            Some((identity, source)) => {
                let entry = persisted_entry(identity, source, now_ms);
                save(store, &entry).await?;
                Ok(entry)
            }
            None => Err(MeshIdentityError::Unresolved),
        }
    }
}

/// Build a cache entry with the standard version + TTL. Centralizes the
/// two construction sites in [`resolve_with`] so they can't drift.
fn persisted_entry(identity: String, source: Source, now_ms: u64) -> CachedIdentity {
    CachedIdentity {
        version: 1,
        identity,
        source,
        resolved_at_ms: now_ms,
        ttl_ms: DEFAULT_TTL_MS,
    }
}

/// Read the cache without resolving. Returns `None` if the file
/// doesn't exist. Used by code paths that want to know "do we have an
/// identity?" without triggering a `gh` shell-out (e.g., status
/// printers).
pub async fn load_cached(
    store: &dyn EventStore,
) -> Result<Option<CachedIdentity>, MeshIdentityError> {
    store
        .load_mesh_identity(MESH_IDENTITY_SCOPE)
        .await?
        .map(CachedIdentity::try_from)
        .transpose()
}

/// Persist the cache.
pub async fn save(store: &dyn EventStore, entry: &CachedIdentity) -> Result<(), MeshIdentityError> {
    store
        .save_mesh_identity(StoredMeshIdentity::from(entry.clone()))
        .await?;
    Ok(())
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::GhApiUser => "gh_api_user",
            Self::GitEmail => "git_email",
            Self::LocalHostUser => "local_host_user",
            Self::Operator => "operator",
        }
    }
}

impl TryFrom<&str> for Source {
    type Error = MeshIdentityError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "gh_api_user" => Ok(Self::GhApiUser),
            "git_email" => Ok(Self::GitEmail),
            "local_host_user" => Ok(Self::LocalHostUser),
            "operator" => Ok(Self::Operator),
            other => Err(MeshIdentityError::UnknownSource(other.to_string())),
        }
    }
}

impl From<CachedIdentity> for StoredMeshIdentity {
    fn from(value: CachedIdentity) -> Self {
        Self {
            scope: MESH_IDENTITY_SCOPE.to_string(),
            identity: value.identity,
            source: value.source.as_str().to_string(),
            resolved_at_ms: value.resolved_at_ms,
            ttl_ms: value.ttl_ms,
        }
    }
}

impl TryFrom<StoredMeshIdentity> for CachedIdentity {
    type Error = MeshIdentityError;

    fn try_from(value: StoredMeshIdentity) -> Result<Self, Self::Error> {
        Ok(Self {
            version: 1,
            identity: value.identity,
            source: Source::try_from(value.source.as_str())?,
            resolved_at_ms: value.resolved_at_ms,
            ttl_ms: value.ttl_ms,
        })
    }
}

/// Default resolver: `gh api user --jq .login` then `git config
/// login stated by the owner. Returns `None` when neither can name the
/// owner — and `None` means unresolved, never a fabricated identity.
fn default_resolver() -> Option<(String, Source)> {
    // The owner naming their own login wins: the escape hatch for a box with
    // no gh (offline, container, CI) that still belongs to a real account.
    if let Ok(pinned) = std::env::var(OWNER_IDENTITY_ENV) {
        let pinned = pinned.trim();
        if !pinned.is_empty() {
            return Some((pinned.to_string(), Source::Operator));
        }
    }
    if let Some(login) = run_command(&["gh", "api", "user", "--jq", ".login"]) {
        if !login.is_empty() {
            return Some((login, Source::GhApiUser));
        }
    }
    None
}

/// Env var by which the owner states their own GitHub login when `gh` is not
/// available on this box. The ONLY substitute for the gh probe, because it is
/// the owner speaking rather than the machine guessing.
pub const OWNER_IDENTITY_ENV: &str = "AIRC_MESH_IDENTITY";

/// Default deadline for resolver shell-outs (gh, git). Bounds
/// `gh api user` / `git config user.email` so a hung or slow
/// gh-auth probe (Windows CI runners, network glitches, gh
/// rate-limit) can't block the whole `airc join` flow.
const RESOLVER_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Run a command and return its trimmed stdout if it exits zero
/// within [`RESOLVER_COMMAND_TIMEOUT`]. `None` on any failure path
/// (command missing, non-zero exit, non-UTF-8 output, timeout) —
/// caller decides what to do.
///
/// Synchronous wait_with_timeout pattern: spawn the child, poll
/// `try_wait` until the deadline. On timeout, kill the child and
/// return None so the caller falls through to the next resolver.
fn run_command(argv: &[&str]) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + RESOLVER_COMMAND_TIMEOUT;
    let output = loop {
        match child.try_wait().ok()? {
            Some(status) => {
                let out = child.wait_with_output().ok()?;
                if !status.success() {
                    return None;
                }
                break out;
            }
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The machine-account airc home (`~/.airc`) — shared by EVERY scope on
/// this machine, so [`machine_id`] resolves to one value per machine
/// rather than per project scope (which would re-fragment the mesh). Not
/// `$AIRC_HOME`: that points at the current project scope, not the machine.
fn machine_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE")) // Windows
        .map(|h| PathBuf::from(h).join(".airc"))
}

/// Stable, machine-wide unique id — persisted once at `~/.airc/machine-id`
/// and reused by every scope. The canonical machine key: it keeps the
/// last-resort local identity DISTINCT across machines (never the
/// colliding `unknown-host`) and is the single key for the account-registry
/// gist name (one gist per machine, regardless of how `hostname` resolves —
/// see `gh::account_registry::writer_key`). If the home is unwritable it
/// degrades to a per-process id — still distinct across machines, just not
/// persisted.
pub(crate) fn machine_id() -> String {
    if let Some(home) = machine_home() {
        let path = home.join("machine-id");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            let trimmed = existing.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        let fresh = Uuid::new_v4().simple().to_string();
        let _ = std::fs::create_dir_all(&home);
        let _ = std::fs::write(&path, &fresh);
        return fresh;
    }
    Uuid::new_v4().simple().to_string()
}

fn now_ms() -> Result<u64, std::time::SystemTimeError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_store::InMemoryEventStore;

    fn mock_gh(value: &'static str) -> impl FnOnce() -> Option<(String, Source)> {
        move || Some((value.to_string(), Source::GhApiUser))
    }

    fn mock_none() -> Option<(String, Source)> {
        None
    }

    #[tokio::test]
    async fn resolve_with_injected_resolver_persists() {
        let store = InMemoryEventStore::new();
        let entry = resolve_with(&store, mock_gh("joelteply"), 1_000)
            .await
            .unwrap();
        assert_eq!(entry.identity, "joelteply");
        assert_eq!(entry.source, Source::GhApiUser);
        assert_eq!(entry.resolved_at_ms, 1_000);
        assert!(load_cached(&store).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn resolve_uses_cache_when_fresh() {
        let store = InMemoryEventStore::new();
        // First resolve writes "alice".
        resolve_with(&store, mock_gh("alice"), 1_000).await.unwrap();
        // Second resolve with a DIFFERENT mock should still see "alice"
        // because the cache is fresh.
        let entry = resolve_with(&store, mock_gh("bob"), 1_500).await.unwrap();
        assert_eq!(entry.identity, "alice", "cache must short-circuit");
    }

    #[tokio::test]
    async fn resolve_re_resolves_after_ttl_expiry() {
        let store = InMemoryEventStore::new();
        resolve_with(&store, mock_gh("alice"), 1_000).await.unwrap();
        // 24h + 1ms past resolution.
        let later = 1_000 + DEFAULT_TTL_MS + 1;
        let entry = resolve_with(&store, mock_gh("bob"), later).await.unwrap();
        assert_eq!(entry.identity, "bob");
    }

    /// what this catches: the island generator. A machine that cannot name
    /// its OWNER must not invent an identity — every room UUID derived from a
    /// fabricated one lands in a namespace only this machine can see, which is
    /// how a node silently forms its own mesh while reporting a healthy join.
    /// Unresolved is loud and terminal; nothing is persisted.
    #[tokio::test]
    async fn resolve_errors_rather_than_inventing_an_owner_identity() {
        let store = InMemoryEventStore::new();
        let error = resolve_with(&store, mock_none, 1_000)
            .await
            .expect_err("no owner identity must not resolve");
        assert!(matches!(error, MeshIdentityError::Unresolved));
        assert!(
            load_cached(&store).await.unwrap().is_none(),
            "a failed resolve must persist NOTHING — a stored guess would \
             outlive the outage and keep deriving private rooms"
        );
    }

    #[tokio::test]
    async fn provisional_git_email_self_heals_to_gh() {
        let store = InMemoryEventStore::new();
        // A scope forked onto git-email because gh missed once.
        save(
            &store,
            &CachedIdentity {
                version: 1,
                identity: "joelteply@yahoo.com".to_string(),
                source: Source::GitEmail,
                resolved_at_ms: 1_000,
                ttl_ms: DEFAULT_TTL_MS,
            },
        )
        .await
        .unwrap();
        // Even though the git-email cache is FRESH, gh answering must win
        // and overwrite it — one identity per machine.
        let entry = resolve_with(&store, mock_gh("joelteply"), 1_500)
            .await
            .unwrap();
        assert_eq!(entry.identity, "joelteply");
        assert_eq!(entry.source, Source::GhApiUser);
        // Persisted, so every later scope/resolve sees the healed login.
        let reloaded = load_cached(&store).await.unwrap().unwrap();
        assert_eq!(reloaded.identity, "joelteply");
        assert_eq!(reloaded.source, Source::GhApiUser);
    }

    /// what this catches: a legacy row minted before this machine could
    /// authenticate (`git_email` / `local_host_user`) is a MACHINE fact, not
    /// the owner's login. Keeping it "for stability" is what stranded bigmama
    /// on her own `#general`. With gh still unreachable it must be refused,
    /// not reused.
    #[tokio::test]
    async fn legacy_provisional_row_is_refused_when_gh_unavailable() {
        let store = InMemoryEventStore::new();
        save(
            &store,
            &CachedIdentity {
                version: 1,
                identity: "joelteply@yahoo.com".to_string(),
                source: Source::GitEmail,
                resolved_at_ms: 1_000,
                ttl_ms: DEFAULT_TTL_MS,
            },
        )
        .await
        .unwrap();
        let error = resolve_with(&store, mock_none, 1_500)
            .await
            .expect_err("a machine-fact identity is not the owner's key");
        assert!(matches!(error, MeshIdentityError::Unresolved));
    }

    /// what this catches: the opposite mistake. A machine that HAS the key and
    /// merely went offline must keep using it — a stale copy of the owner's
    /// login is still the owner's login, and erroring here would drop a
    /// converged machine off its own mesh over a transient gh outage.
    #[tokio::test]
    async fn expired_gh_login_survives_an_unreachable_gh() {
        let store = InMemoryEventStore::new();
        resolve_with(&store, mock_gh("joelteply"), 1_000)
            .await
            .unwrap();
        let entry = resolve_with(&store, mock_none, 1_000 + DEFAULT_TTL_MS + 1)
            .await
            .expect("a cached gh login must survive an outage");
        assert_eq!(entry.identity, "joelteply");
        assert_eq!(entry.source, Source::GhApiUser);
    }

    #[tokio::test]
    async fn operator_override_is_never_overwritten_by_gh() {
        let store = InMemoryEventStore::new();
        save(
            &store,
            &CachedIdentity {
                version: 1,
                identity: "pinned-id".to_string(),
                source: Source::Operator,
                resolved_at_ms: 1,
                ttl_ms: DEFAULT_TTL_MS,
            },
        )
        .await
        .unwrap();
        let entry = resolve_with(&store, mock_gh("joelteply"), 9_999_999)
            .await
            .unwrap();
        assert_eq!(entry.identity, "pinned-id");
        assert_eq!(entry.source, Source::Operator);
    }

    #[test]
    fn as_mesh_identity_round_trips_to_typed_value() {
        let entry = CachedIdentity {
            version: 1,
            identity: "joelteply".to_string(),
            source: Source::GhApiUser,
            resolved_at_ms: 0,
            ttl_ms: DEFAULT_TTL_MS,
        };
        assert_eq!(entry.as_mesh_identity().as_str(), "joelteply");
    }

    #[tokio::test]
    async fn load_cached_returns_none_when_store_has_no_row() {
        let store = InMemoryEventStore::new();
        assert!(load_cached(&store).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn load_cached_rejects_unknown_source() {
        let store = InMemoryEventStore::new();
        store
            .save_mesh_identity(StoredMeshIdentity {
                scope: MESH_IDENTITY_SCOPE.to_string(),
                identity: "alice".to_string(),
                source: "surprise".to_string(),
                resolved_at_ms: 0,
                ttl_ms: DEFAULT_TTL_MS,
            })
            .await
            .unwrap();
        let err = load_cached(&store).await.unwrap_err();
        assert!(
            matches!(err, MeshIdentityError::UnknownSource(ref source) if source == "surprise")
        );
    }

    #[test]
    fn is_expired_uses_saturating_sub_for_clock_skew() {
        let entry = CachedIdentity {
            version: 1,
            identity: "x".to_string(),
            source: Source::GhApiUser,
            // Future-dated resolved_at — saturating_sub yields 0,
            // so is_expired returns 0 >= ttl which is false unless
            // ttl is 0. Keep clock skew from breaking cache.
            resolved_at_ms: 1_000_000,
            ttl_ms: DEFAULT_TTL_MS,
        };
        assert!(!entry.is_expired(500_000));
    }

    #[tokio::test]
    async fn save_load_round_trip_preserves_entry() {
        let store = InMemoryEventStore::new();
        let entry = CachedIdentity {
            version: 1,
            identity: "joelteply".to_string(),
            source: Source::GhApiUser,
            resolved_at_ms: 42,
            ttl_ms: DEFAULT_TTL_MS,
        };
        save(&store, &entry).await.unwrap();
        let loaded = load_cached(&store).await.unwrap().unwrap();
        assert_eq!(loaded, entry);
    }
}
