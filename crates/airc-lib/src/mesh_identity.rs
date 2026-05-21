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
//! 1. **Cached value** in `<home>/mesh_identity.json`, if fresh
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
//! Persisted to `<home>/mesh_identity.json`. Schema version 1.
//! Re-resolution kicks in after `DEFAULT_TTL_MS`; cache hits never
//! shell out, so ten local scopes opening at once produce at most
//! one `gh` call.
//!
//! ## Test injection
//!
//! [`resolve_with`] takes a closure that produces the raw identity
//! string, sidestepping the shell-out. Production code calls
//! [`resolve`] which uses the gh+git fallback resolver. Tests pass
//! a fixed-string closure.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::subscriptions::MeshIdentity;

const IDENTITY_FILENAME: &str = "mesh_identity.json";
const IDENTITY_VERSION: u32 = 1;
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
    Io(std::io::Error),
    Json(serde_json::Error),
    Clock(std::time::SystemTimeError),
    SchemaVersionMismatch { found: u32, expected: u32 },
}

impl std::fmt::Display for MeshIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "mesh identity I/O: {error}"),
            Self::Json(error) => write!(f, "mesh identity JSON: {error}"),
            Self::Clock(error) => write!(f, "mesh identity clock: {error}"),
            Self::SchemaVersionMismatch { found, expected } => {
                write!(f, "mesh_identity.json version {found}, expected {expected}")
            }
        }
    }
}

impl std::error::Error for MeshIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::SchemaVersionMismatch { .. } => None,
        }
    }
}

impl From<std::io::Error> for MeshIdentityError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for MeshIdentityError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::time::SystemTimeError> for MeshIdentityError {
    fn from(value: std::time::SystemTimeError) -> Self {
        Self::Clock(value)
    }
}

/// On-disk path for the cache.
pub fn path_in(home: &Path) -> PathBuf {
    home.join(IDENTITY_FILENAME)
}

/// Resolve via the default fallback chain (gh → git email → local
/// hostname/user) and persist. Most callers want this.
pub fn resolve(home: &Path) -> Result<CachedIdentity, MeshIdentityError> {
    resolve_with(home, default_resolver, now_ms()?)
}

/// Resolve with an injected resolver closure. The closure returns
/// `Some((identity, source))` on success, `None` if it has nothing
/// to contribute and the LocalHostUser fallback should be used.
///
/// Used by tests to bypass `gh` / `git` shell-outs and by production
/// callers via [`resolve`].
pub fn resolve_with<F>(
    home: &Path,
    resolver: F,
    now_ms: u64,
) -> Result<CachedIdentity, MeshIdentityError>
where
    F: FnOnce() -> Option<(String, Source)>,
{
    if let Some(cached) = load_cached(home)? {
        if !cached.is_expired(now_ms) {
            return Ok(cached);
        }
    }

    let (identity, source) = match resolver() {
        Some(pair) => pair,
        None => (local_fallback_identity(), Source::LocalHostUser),
    };

    let entry = CachedIdentity {
        version: IDENTITY_VERSION,
        identity,
        source,
        resolved_at_ms: now_ms,
        ttl_ms: DEFAULT_TTL_MS,
    };
    save(home, &entry)?;
    Ok(entry)
}

/// Read the cache without resolving. Returns `None` if the file
/// doesn't exist. Used by code paths that want to know "do we have an
/// identity?" without triggering a `gh` shell-out (e.g., status
/// printers).
pub fn load_cached(home: &Path) -> Result<Option<CachedIdentity>, MeshIdentityError> {
    let path = path_in(home);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    let entry: CachedIdentity = serde_json::from_str(&text)?;
    if entry.version != IDENTITY_VERSION {
        return Err(MeshIdentityError::SchemaVersionMismatch {
            found: entry.version,
            expected: IDENTITY_VERSION,
        });
    }
    Ok(Some(entry))
}

/// Persist the cache.
pub fn save(home: &Path, entry: &CachedIdentity) -> Result<(), MeshIdentityError> {
    std::fs::create_dir_all(home)?;
    let path = path_in(home);
    let text = serde_json::to_string_pretty(entry)?;
    std::fs::write(&path, text)?;
    set_owner_only_permissions(&path)?;
    Ok(())
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

/// Run a command and return its trimmed stdout if it exits zero.
/// `None` on any failure path (command missing, non-zero exit,
/// non-UTF-8 output) — caller decides what to do.
fn run_command(argv: &[&str]) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
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
    use tempfile::tempdir;

    fn mock_gh(value: &'static str) -> impl FnOnce() -> Option<(String, Source)> {
        move || Some((value.to_string(), Source::GhApiUser))
    }

    fn mock_none() -> Option<(String, Source)> {
        None
    }

    #[test]
    fn resolve_with_injected_resolver_persists() {
        let dir = tempdir().unwrap();
        let entry = resolve_with(dir.path(), mock_gh("joelteply"), 1_000).unwrap();
        assert_eq!(entry.identity, "joelteply");
        assert_eq!(entry.source, Source::GhApiUser);
        assert_eq!(entry.resolved_at_ms, 1_000);
        assert!(path_in(dir.path()).exists());
    }

    #[test]
    fn resolve_uses_cache_when_fresh() {
        let dir = tempdir().unwrap();
        // First resolve writes "alice".
        resolve_with(dir.path(), mock_gh("alice"), 1_000).unwrap();
        // Second resolve with a DIFFERENT mock should still see "alice"
        // because the cache is fresh.
        let entry = resolve_with(dir.path(), mock_gh("bob"), 1_500).unwrap();
        assert_eq!(entry.identity, "alice", "cache must short-circuit");
    }

    #[test]
    fn resolve_re_resolves_after_ttl_expiry() {
        let dir = tempdir().unwrap();
        resolve_with(dir.path(), mock_gh("alice"), 1_000).unwrap();
        // 24h + 1ms past resolution.
        let later = 1_000 + DEFAULT_TTL_MS + 1;
        let entry = resolve_with(dir.path(), mock_gh("bob"), later).unwrap();
        assert_eq!(entry.identity, "bob");
    }

    #[test]
    fn resolve_falls_back_to_local_when_resolver_yields_none() {
        let dir = tempdir().unwrap();
        let entry = resolve_with(dir.path(), mock_none, 1_000).unwrap();
        assert_eq!(entry.source, Source::LocalHostUser);
        assert!(entry.identity.starts_with("local:"));
        // Fallback is the SAME on a given machine — second resolve
        // (fresh cache) returns the cached fallback value too.
        let entry2 = resolve_with(dir.path(), mock_none, 1_100).unwrap();
        assert_eq!(entry2.identity, entry.identity);
    }

    #[test]
    fn as_mesh_identity_round_trips_to_typed_value() {
        let entry = CachedIdentity {
            version: IDENTITY_VERSION,
            identity: "joelteply".to_string(),
            source: Source::GhApiUser,
            resolved_at_ms: 0,
            ttl_ms: DEFAULT_TTL_MS,
        };
        assert_eq!(entry.as_mesh_identity().as_str(), "joelteply");
    }

    #[test]
    fn load_cached_returns_none_when_file_absent() {
        let dir = tempdir().unwrap();
        assert!(load_cached(dir.path()).unwrap().is_none());
    }

    #[test]
    fn load_cached_rejects_wrong_schema_version() {
        let dir = tempdir().unwrap();
        let bad = serde_json::json!({
            "version": 999,
            "identity": "alice",
            "source": "gh_api_user",
            "resolved_at_ms": 0,
            "ttl_ms": DEFAULT_TTL_MS,
        });
        std::fs::write(path_in(dir.path()), serde_json::to_string(&bad).unwrap()).unwrap();
        let err = load_cached(dir.path()).unwrap_err();
        assert!(matches!(
            err,
            MeshIdentityError::SchemaVersionMismatch {
                found: 999,
                expected: 1
            }
        ));
    }

    #[test]
    fn is_expired_uses_saturating_sub_for_clock_skew() {
        let entry = CachedIdentity {
            version: IDENTITY_VERSION,
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

    #[test]
    fn save_load_round_trip_preserves_entry() {
        let dir = tempdir().unwrap();
        let entry = CachedIdentity {
            version: IDENTITY_VERSION,
            identity: "joelteply".to_string(),
            source: Source::GhApiUser,
            resolved_at_ms: 42,
            ttl_ms: DEFAULT_TTL_MS,
        };
        save(dir.path(), &entry).unwrap();
        let loaded = load_cached(dir.path()).unwrap().unwrap();
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
