//! Mesh identity resolution — who is "us" on this machine?
//!
//! Resolves the user's authenticated Git/GitHub identity once per
//! machine and caches it for the [`crate::subscriptions::SubscriptionSet`]
//! RoomId derivation. Without this, every scope on every user's
//! machine falls back to [`crate::subscriptions::MeshIdentity::unset`],
//! which is fine for tests but collides every user's `#general` onto
//! the same `RoomId` — a privacy bug.
//!
//! ## Resolution order
//!
//! 1. **Cached value** in the `mesh_identity` ORM table, if fresh
//!    (`resolved_at_ms` within [`DEFAULT_TTL_MS`]).
//! 2. **`gh api user --jq .login`** — the canonical GitHub identity
//!    when `gh` is installed and authenticated.
//! 3. **`git config user.email`** fallback when `gh` isn't available.
//! 4. **`local:<host>:<user>`** last-resort deterministic local
//!    identity. Warned in a side-channel (callers can read it from
//!    the persisted [`Source`] field) so the operator knows the
//!    machine couldn't authenticate against GitHub.
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

use std::process::Command;

use airc_store::{EventStore, StoredMeshIdentity};
use serde::{Deserialize, Serialize};

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
}

impl std::fmt::Display for MeshIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "mesh identity store: {error}"),
            Self::Clock(error) => write!(f, "mesh identity clock: {error}"),
            Self::UnknownSource(source) => write!(f, "unknown mesh identity source: {source}"),
        }
    }
}

impl std::error::Error for MeshIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::UnknownSource(_) => None,
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

/// Resolve via the default fallback chain (gh → git email → local
/// hostname/user) and persist. Most callers want this.
pub async fn resolve(store: &dyn EventStore) -> Result<CachedIdentity, MeshIdentityError> {
    resolve_with(store, default_resolver, now_ms()?).await
}

/// Resolve with an injected resolver closure. The closure returns
/// `Some((identity, source))` on success, `None` if it has nothing
/// to contribute and the LocalHostUser fallback should be used.
///
/// Used by tests to bypass `gh` / `git` shell-outs and by production
/// callers via [`resolve`].
pub async fn resolve_with<F>(
    store: &dyn EventStore,
    resolver: F,
    now_ms: u64,
) -> Result<CachedIdentity, MeshIdentityError>
where
    F: FnOnce() -> Option<(String, Source)>,
{
    if let Some(cached) = load_cached(store).await? {
        // Operator-source caches are trusted as-is and never expire —
        // they were explicitly set by the user (env var, CLI override,
        // test seed). Treating them as TTL-bounded forces a fall-through
        // to gh/git shell-outs that the operator was trying to avoid.
        // Caught when the e2e join_without_args test seeded
        // `resolved_at_ms: 1` for hermetic mesh-identity in CI, and
        // Windows runners hung on the gh shell-out because the
        // tiny-resolved_at made is_expired return true on every call.
        if cached.source == Source::Operator || !cached.is_expired(now_ms) {
            return Ok(cached);
        }
    }

    let (identity, source) = match resolver() {
        Some(pair) => pair,
        None => (local_fallback_identity(), Source::LocalHostUser),
    };

    let entry = CachedIdentity {
        version: 1,
        identity,
        source,
        resolved_at_ms: now_ms,
        ttl_ms: DEFAULT_TTL_MS,
    };
    save(store, &entry).await?;
    Ok(entry)
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
/// user.email`. Returns `None` if neither succeeds — caller falls
/// back to `LocalHostUser`.
fn default_resolver() -> Option<(String, Source)> {
    if let Some(login) = run_command(&["gh", "api", "user", "--jq", ".login"]) {
        if !login.is_empty() {
            return Some((login, Source::GhApiUser));
        }
    }
    if let Some(email) = run_command(&["git", "config", "user.email"]) {
        if !email.is_empty() {
            return Some((email, Source::GitEmail));
        }
    }
    None
}

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

/// Last-resort identity: `local:<host>:<user>`. Deterministic per
/// machine+user but does NOT converge across machines or with the
/// operator's `gh` identity — the operator should know about this.
fn local_fallback_identity() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| run_command(&["hostname", "-s"]))
        .unwrap_or_else(|| "unknown-host".to_string());
    let user = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .unwrap_or_else(|| "unknown-user".to_string());
    format!("local:{host}:{user}")
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

    #[tokio::test]
    async fn resolve_falls_back_to_local_when_resolver_yields_none() {
        let store = InMemoryEventStore::new();
        let entry = resolve_with(&store, mock_none, 1_000).await.unwrap();
        assert_eq!(entry.source, Source::LocalHostUser);
        assert!(entry.identity.starts_with("local:"));
        // Fallback is the SAME on a given machine — second resolve
        // (fresh cache) returns the cached fallback value too.
        let entry2 = resolve_with(&store, mock_none, 1_100).await.unwrap();
        assert_eq!(entry2.identity, entry.identity);
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

    #[test]
    fn local_fallback_is_deterministic_for_same_env() {
        // Without setting env, fallback should at least be a stable
        // string for the duration of the test process.
        let a = local_fallback_identity();
        let b = local_fallback_identity();
        assert_eq!(a, b);
        assert!(a.starts_with("local:"));
    }
}
