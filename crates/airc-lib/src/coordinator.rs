//! Machine-global account coordinator — Gap 8 from
//! [`ACCOUNT-MESH-JOIN-CONTRACT.md`].
//!
//! Every machine needs one coordinator per Git/GitHub mesh identity.
//! It is the local source of truth for "which scopes on this machine
//! are alive on the account mesh, what channels are they subscribed
//! to, and when did they last heartbeat." Without it, every tab /
//! terminal / agent independently probes GitHub on join — which made
//! the legacy gh-gist data plane fall over under concurrent joins.
//!
//! ## State layout
//!
//! ```text
//! ~/.airc/accounts/<mesh-identity>/
//!   beacons/
//!     <peer-id>.json           # per-scope presence beacon
//!   refresh.lock                # singleflight sentinel for remote refresh
//! ```
//!
//! Each scope (Claude tab, Codex tab, persona instance, daemon, etc.)
//! writes ONE beacon under `beacons/<peer-id>.json`. The peer-id is
//! the scope's stable identifier (from `Airc::peer_id`), so two
//! processes from the same scope coalesce; two scopes never collide.
//!
//! The coordinator never mutates other scopes' beacons. It only:
//!
//! - reads all beacons in the directory to compute a `CoordinatorSnapshot`;
//! - writes / refreshes the caller's own beacon via `publish`;
//! - drains expired beacons via `drain_stale` (separate verb so callers
//!   opt in to destructive action — "drains are core" rule).
//!
//! ## TTL and singleflight
//!
//! Two distinct concerns:
//!
//! 1. **Beacon TTL** — a beacon older than `heartbeat_ttl_ms` is
//!    considered stale. Stale beacons appear in
//!    [`CoordinatorSnapshot::stale`] separately from live ones. Stale
//!    beacons stay on disk until `drain_stale` runs, so a transient
//!    crash doesn't immediately purge the record (recovery still
//!    sees the old subscriptions).
//! 2. **Remote-refresh singleflight** — when a join needs the
//!    rare-and-expensive remote registry refresh (GitHub gist pull),
//!    [`try_acquire_refresh_lock`] takes the `refresh.lock` sentinel.
//!    Concurrent joins return `HeldFresh` and re-use the snapshot the
//!    lock-holder produces. Without this, ten local agents starting
//!    `airc join` simultaneously would each hammer GitHub.
//!
//! ## Scope: what's in this PR
//!
//! The pure local presence/beacon machinery + singleflight primitive.
//! Wiring into `Airc::join` / wrapper / monitor / hooks comes in
//! follow-up PRs, per Joel's "no monitor/hook changes in first
//! coordinator PR unless API is stable" boundary.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::coordinator_lock::RefreshLock;
use crate::fs_permissions;
use crate::subscriptions::{ChannelName, MeshIdentity};

const BEACON_VERSION: u32 = 1;
const ACCOUNTS_DIR: &str = "accounts";
const BEACONS_DIR: &str = "beacons";
const REFRESH_LOCK: &str = "refresh.lock";
const REFRESH_TAKEOVER_LOCK: &str = "refresh.takeover";

/// Default beacon staleness threshold: 60s. A scope that hasn't
/// heartbeated within this window is considered stale (process dead
/// or wedged). Tuned to be longer than a normal join's startup cost
/// but short enough that a crashed scope doesn't linger in
/// "alive" state for the full session.
pub const DEFAULT_HEARTBEAT_TTL_MS: u64 = 60_000;

/// Default remote-refresh debounce: 5s. After a successful remote
/// refresh, subsequent joins within this window skip the network
/// path entirely and read the cached snapshot. Even if the lock is
/// available, no caller will take it within this window without an
/// explicit override.
pub const DEFAULT_REFRESH_INTERVAL_MS: u64 = 5_000;

/// Coordinator behaviour tunables. `Default` matches the constants
/// above; tests and special callers can override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorConfig {
    pub heartbeat_ttl_ms: u64,
    pub refresh_interval_ms: u64,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            heartbeat_ttl_ms: DEFAULT_HEARTBEAT_TTL_MS,
            refresh_interval_ms: DEFAULT_REFRESH_INTERVAL_MS,
        }
    }
}

/// One scope's published presence + subscriptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceBeacon {
    pub version: u32,
    pub peer_id: PeerId,
    /// AIRC_HOME of the publishing scope (per-project or HOME). Used
    /// to distinguish scopes that share a mesh identity but live in
    /// different dirs (e.g., a CLI tab in `~/.airc/` vs. a project
    /// agent in `~/Development/foo/.airc/`).
    pub scope_home: PathBuf,
    pub subscribed_channels: Vec<ChannelName>,
    /// Process id at publish time. Inspectable but never load-bearing
    /// for the freshness decision (heartbeat_at_ms is the truth).
    /// PID collisions across reboots make pid-only liveness checks
    /// unsafe.
    pub pid: u32,
    pub published_at_ms: u64,
    pub heartbeat_at_ms: u64,
}

impl PresenceBeacon {
    /// Heartbeat freshness: true if the beacon's `heartbeat_at_ms`
    /// is within `ttl_ms` of `now_ms`. Saturating-sub guards against
    /// future-dated clocks (treat as fresh rather than underflow).
    pub fn is_fresh(&self, now_ms: u64, ttl_ms: u64) -> bool {
        now_ms.saturating_sub(self.heartbeat_at_ms) < ttl_ms
    }
}

/// Read-only snapshot of all beacons under one mesh identity. Built
/// by [`snapshot`] in O(n) over the beacon files. `live` and
/// `stale` are partitioned by [`CoordinatorConfig::heartbeat_ttl_ms`]
/// so callers can decide whether to act on stale entries without
/// re-walking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorSnapshot {
    pub mesh_identity: MeshIdentity,
    pub root: PathBuf,
    pub live: Vec<PresenceBeacon>,
    pub stale: Vec<PresenceBeacon>,
    /// Channels mentioned by at least one live beacon. Useful for
    /// "which rooms are active on this machine right now" status.
    pub live_channels: Vec<ChannelName>,
    pub fetched_at_ms: u64,
}

/// What can go wrong reading/writing the coordinator state.
#[derive(Debug)]
pub enum CoordinatorError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// A beacon file existed but its schema version didn't match. We
    /// surface this rather than silently skip so the operator sees
    /// the foreign-version surface.
    SchemaVersionMismatch {
        path: PathBuf,
        found: u32,
        expected: u32,
    },
}

impl std::fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "coordinator I/O: {error}"),
            Self::Json(error) => write!(f, "coordinator JSON: {error}"),
            Self::SchemaVersionMismatch {
                path,
                found,
                expected,
            } => write!(
                f,
                "beacon {} version {found}, expected {expected}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CoordinatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::SchemaVersionMismatch { .. } => None,
        }
    }
}

impl From<std::io::Error> for CoordinatorError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for CoordinatorError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Compute the account-root directory for a mesh identity:
/// `<airc_home>/accounts/<mesh-identity>/`.
///
/// `airc_home` is conventionally `~/.airc/` but any directory works
/// (tests pass a tempdir). The mesh-identity is path-sanitized so
/// e.g. an email-like identity `joel@example.com` becomes a safe
/// directory component.
pub fn account_root(airc_home: &Path, identity: &MeshIdentity) -> PathBuf {
    airc_home
        .join(ACCOUNTS_DIR)
        .join(sanitize_identity(identity.as_str()))
}

fn sanitize_identity(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn beacons_dir(airc_home: &Path, identity: &MeshIdentity) -> PathBuf {
    account_root(airc_home, identity).join(BEACONS_DIR)
}

fn beacon_path(airc_home: &Path, identity: &MeshIdentity, peer_id: PeerId) -> PathBuf {
    beacons_dir(airc_home, identity).join(format!("{peer_id}.json"))
}

fn refresh_lock_path(airc_home: &Path, identity: &MeshIdentity) -> PathBuf {
    account_root(airc_home, identity).join(REFRESH_LOCK)
}

fn refresh_takeover_lock_path(airc_home: &Path, identity: &MeshIdentity) -> PathBuf {
    account_root(airc_home, identity).join(REFRESH_TAKEOVER_LOCK)
}

/// Publish the caller's beacon. Atomic via write-tmp + rename so a
/// concurrent reader never sees a partial file.
pub fn publish(
    airc_home: &Path,
    identity: &MeshIdentity,
    beacon: &PresenceBeacon,
) -> Result<(), CoordinatorError> {
    let dir = beacons_dir(airc_home, identity);
    fs::create_dir_all(&dir)?;
    let final_path = beacon_path(airc_home, identity, beacon.peer_id);
    let tmp_path = dir.join(format!(".{}.tmp", beacon.peer_id));
    let text = serde_json::to_string_pretty(beacon)?;
    fs::write(&tmp_path, text)?;
    fs_permissions::set_owner_only(&tmp_path)?;
    fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Read the caller's own beacon, if it exists. `None` distinguishes
/// "no prior publish" from "publish exists with stale heartbeat" —
/// the caller computes freshness from the returned beacon's
/// `is_fresh`.
pub fn load_own_beacon(
    airc_home: &Path,
    identity: &MeshIdentity,
    peer_id: PeerId,
) -> Result<Option<PresenceBeacon>, CoordinatorError> {
    let path = beacon_path(airc_home, identity, peer_id);
    if !path.exists() {
        return Ok(None);
    }
    let beacon = read_beacon(&path)?;
    Ok(Some(beacon))
}

/// Build a snapshot of all beacons for a mesh identity. Beacons
/// whose `heartbeat_at_ms` is within `config.heartbeat_ttl_ms` of
/// `now_ms` land in `live`; the rest in `stale`.
pub fn snapshot(
    airc_home: &Path,
    identity: &MeshIdentity,
    config: &CoordinatorConfig,
    now_ms: u64,
) -> Result<CoordinatorSnapshot, CoordinatorError> {
    let root = account_root(airc_home, identity);
    let beacons_dir = root.join(BEACONS_DIR);
    let mut live = Vec::new();
    let mut stale = Vec::new();
    if beacons_dir.exists() {
        for entry in fs::read_dir(&beacons_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // Skip tmp/dot files left mid-rename.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            let beacon = read_beacon(&path)?;
            if beacon.is_fresh(now_ms, config.heartbeat_ttl_ms) {
                live.push(beacon);
            } else {
                stale.push(beacon);
            }
        }
    }
    // Stable ordering for deterministic display + diffs.
    // PeerId is a UUID wrapper without Ord; sort by string form for
    // deterministic display + diffs across runs.
    live.sort_by_key(|b| b.peer_id.to_string());
    stale.sort_by_key(|b| b.peer_id.to_string());
    let live_channels = unique_channels_in(&live);
    Ok(CoordinatorSnapshot {
        mesh_identity: identity.clone(),
        root,
        live,
        stale,
        live_channels,
        fetched_at_ms: now_ms,
    })
}

/// Delete beacon files for all entries currently in
/// [`CoordinatorSnapshot::stale`]. Best-effort — missing files
/// (raced with another draining process) aren't an error. Returns
/// the count of beacons removed.
///
/// Separate from `snapshot` so callers opt in to destructive action.
pub fn drain_stale(
    airc_home: &Path,
    identity: &MeshIdentity,
    snapshot: &CoordinatorSnapshot,
) -> Result<usize, CoordinatorError> {
    let mut removed = 0;
    for beacon in &snapshot.stale {
        let path = beacon_path(airc_home, identity, beacon.peer_id);
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            // Concurrent drain already won — fine.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(removed)
}

/// Outcome of attempting to acquire the remote-refresh lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshLockOutcome {
    /// Caller is the lock-holder for `now_ms`. They must perform the
    /// refresh and call `release_refresh_lock` when done.
    Acquired,
    /// Lock is held by another caller whose acquisition is within
    /// `refresh_interval_ms`; skip the refresh and re-use snapshot.
    /// The `held_at_ms` is the lock holder's published timestamp so
    /// the skipping caller can decide whether to wait or proceed.
    HeldFresh { held_at_ms: u64 },
}

/// Try to acquire the remote-refresh lock. Singleflight pattern:
/// only one caller at a time should hammer the remote registry
/// (GitHub gist), so subsequent callers within
/// `refresh_interval_ms` see `HeldFresh` and re-use the lock-holder's
/// snapshot.
///
/// Atomicity is provided by `OpenOptions::create_new(true)` —
/// `O_CREAT|O_EXCL` semantics on POSIX, the equivalent on Windows.
/// Exactly one caller wins the creation; others get
/// `ErrorKind::AlreadyExists` and inspect the existing lock to
/// decide fresh vs stale.
///
/// Stale-takeover path: when the existing lock's `held_at_ms` is
/// past `refresh_interval_ms`, the loser first acquires a second
/// exclusive `refresh.takeover` sentinel. Only that takeover holder
/// may remove and replace the stale `refresh.lock`. Without this,
/// several stale readers can delete each other's newly-created fresh
/// lock on Windows/POSIX under contention. The lock adapter uses a
/// bounded retry loop so an adversary can't pin a caller.
///
/// (Earlier revision of this function did a read-then-write without
/// any atomicity primitive — two concurrent callers could both see
/// "no lock" and both succeed, violating singleflight. Fixed in
/// response to PR #850 review.)
pub fn try_acquire_refresh_lock(
    airc_home: &Path,
    identity: &MeshIdentity,
    config: &CoordinatorConfig,
    now_ms: u64,
    holder_pid: u32,
) -> Result<RefreshLockOutcome, CoordinatorError> {
    let root = account_root(airc_home, identity);
    fs::create_dir_all(&root)?;
    RefreshLock::new(
        refresh_lock_path(airc_home, identity),
        refresh_takeover_lock_path(airc_home, identity),
        config.refresh_interval_ms,
        now_ms,
        holder_pid,
    )?
    .acquire()
}

/// Release the refresh lock — best-effort delete. Idempotent: a
/// missing lock file is not an error (e.g., concurrent drain, or
/// another process already took over after our holder window
/// expired).
pub fn release_refresh_lock(
    airc_home: &Path,
    identity: &MeshIdentity,
) -> Result<(), CoordinatorError> {
    let path = refresh_lock_path(airc_home, identity);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_beacon(path: &Path) -> Result<PresenceBeacon, CoordinatorError> {
    let text = fs::read_to_string(path)?;
    let beacon: PresenceBeacon = serde_json::from_str(&text)?;
    if beacon.version != BEACON_VERSION {
        return Err(CoordinatorError::SchemaVersionMismatch {
            path: path.to_path_buf(),
            found: beacon.version,
            expected: BEACON_VERSION,
        });
    }
    Ok(beacon)
}

fn unique_channels_in(beacons: &[PresenceBeacon]) -> Vec<ChannelName> {
    let mut by_name: HashMap<String, ChannelName> = HashMap::new();
    for beacon in beacons {
        for channel in &beacon.subscribed_channels {
            by_name
                .entry(channel.as_str().to_string())
                .or_insert_with(|| channel.clone());
        }
    }
    let mut out: Vec<ChannelName> = by_name.into_values().collect();
    out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    out
}

/// Convenience: construct a presence beacon at a given timestamp.
/// Most callers compose this themselves; provided so the common case
/// (publish-now) is one line.
pub fn beacon_now(
    peer_id: PeerId,
    scope_home: PathBuf,
    subscribed_channels: Vec<ChannelName>,
    pid: u32,
    now_ms: u64,
) -> PresenceBeacon {
    PresenceBeacon {
        version: BEACON_VERSION,
        peer_id,
        scope_home,
        subscribed_channels,
        pid,
        published_at_ms: now_ms,
        heartbeat_at_ms: now_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn id() -> MeshIdentity {
        MeshIdentity::new("joelteply")
    }

    fn other_id() -> MeshIdentity {
        MeshIdentity::new("someone-else")
    }

    fn peer() -> PeerId {
        PeerId::from_uuid(Uuid::new_v4())
    }

    fn channel(name: &str) -> ChannelName {
        ChannelName::new(name).unwrap()
    }

    fn make_beacon(now_ms: u64, channels: Vec<&str>) -> PresenceBeacon {
        beacon_now(
            peer(),
            PathBuf::from("/tmp/x/.airc"),
            channels.into_iter().map(channel).collect(),
            12345,
            now_ms,
        )
    }

    #[test]
    fn account_root_partitions_by_identity() {
        let home = PathBuf::from("/tmp/x/.airc");
        let a = account_root(&home, &id());
        let b = account_root(&home, &other_id());
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("joelteply"));
    }

    #[test]
    fn account_root_sanitizes_unsafe_chars() {
        let home = PathBuf::from("/tmp/x/.airc");
        let identity = MeshIdentity::new("joel@example.com");
        let root = account_root(&home, &identity);
        let last = root.file_name().unwrap().to_string_lossy().into_owned();
        // `@` is not in [a-z0-9-_.] so it becomes `-`.
        assert_eq!(last, "joel-example.com");
    }

    #[test]
    fn publish_then_load_own_beacon_round_trips() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mesh = id();
        let beacon = make_beacon(1_000, vec!["general", "cambriantech"]);

        publish(home, &mesh, &beacon).unwrap();
        let loaded = load_own_beacon(home, &mesh, beacon.peer_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded, beacon);
    }

    #[test]
    fn load_own_beacon_returns_none_when_absent() {
        let dir = tempdir().unwrap();
        let loaded = load_own_beacon(dir.path(), &id(), peer()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn snapshot_partitions_live_and_stale_by_ttl() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mesh = id();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 100,
        };

        let fresh = make_beacon(950, vec!["general"]);
        let stale = make_beacon(0, vec!["cambriantech"]);
        publish(home, &mesh, &fresh).unwrap();
        publish(home, &mesh, &stale).unwrap();

        let snap = snapshot(home, &mesh, &cfg, 1_000).unwrap();
        assert_eq!(snap.live.len(), 1, "fresh beacon should be live");
        assert_eq!(snap.stale.len(), 1, "old beacon should be stale");
        assert_eq!(snap.live[0].peer_id, fresh.peer_id);
        assert_eq!(snap.stale[0].peer_id, stale.peer_id);
    }

    #[test]
    fn snapshot_aggregates_live_channels_deduplicated() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mesh = id();
        let cfg = CoordinatorConfig::default();

        let a = make_beacon(1_000, vec!["general", "cambriantech"]);
        let b = make_beacon(1_000, vec!["general", "ideem"]);
        publish(home, &mesh, &a).unwrap();
        publish(home, &mesh, &b).unwrap();

        let snap = snapshot(home, &mesh, &cfg, 1_000).unwrap();
        let names: Vec<&str> = snap.live_channels.iter().map(ChannelName::as_str).collect();
        assert_eq!(names, vec!["cambriantech", "general", "ideem"]);
    }

    #[test]
    fn snapshot_empty_when_no_beacons() {
        let dir = tempdir().unwrap();
        let snap = snapshot(dir.path(), &id(), &CoordinatorConfig::default(), 0).unwrap();
        assert!(snap.live.is_empty());
        assert!(snap.stale.is_empty());
        assert!(snap.live_channels.is_empty());
    }

    #[test]
    fn snapshot_isolates_identities() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let cfg = CoordinatorConfig::default();
        let mine = make_beacon(1_000, vec!["general"]);
        let theirs = make_beacon(1_000, vec!["general"]);
        publish(home, &id(), &mine).unwrap();
        publish(home, &other_id(), &theirs).unwrap();

        let my_snap = snapshot(home, &id(), &cfg, 1_000).unwrap();
        let their_snap = snapshot(home, &other_id(), &cfg, 1_000).unwrap();
        assert_eq!(my_snap.live.len(), 1);
        assert_eq!(their_snap.live.len(), 1);
        assert_eq!(my_snap.live[0].peer_id, mine.peer_id);
        assert_eq!(their_snap.live[0].peer_id, theirs.peer_id);
    }

    #[test]
    fn drain_stale_removes_only_stale_files() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mesh = id();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 100,
        };
        let fresh = make_beacon(950, vec!["general"]);
        let stale = make_beacon(0, vec!["general"]);
        publish(home, &mesh, &fresh).unwrap();
        publish(home, &mesh, &stale).unwrap();

        let snap = snapshot(home, &mesh, &cfg, 1_000).unwrap();
        let removed = drain_stale(home, &mesh, &snap).unwrap();
        assert_eq!(removed, 1);

        // Re-snapshot: only the fresh beacon should remain.
        let after = snapshot(home, &mesh, &cfg, 1_000).unwrap();
        assert_eq!(after.live.len(), 1);
        assert_eq!(after.stale.len(), 0);
    }

    #[test]
    fn publish_is_idempotent_via_atomic_rename() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mesh = id();
        let peer_id = peer();
        let first = PresenceBeacon {
            heartbeat_at_ms: 1_000,
            ..beacon_now(
                peer_id,
                PathBuf::from("/tmp/x/.airc"),
                vec![channel("general")],
                100,
                1_000,
            )
        };
        let second = PresenceBeacon {
            heartbeat_at_ms: 2_000,
            ..first.clone()
        };
        publish(home, &mesh, &first).unwrap();
        publish(home, &mesh, &second).unwrap();
        let loaded = load_own_beacon(home, &mesh, peer_id).unwrap().unwrap();
        assert_eq!(loaded.heartbeat_at_ms, 2_000, "second publish wins");
    }

    #[test]
    fn refresh_lock_first_caller_acquires() {
        let dir = tempdir().unwrap();
        let outcome =
            try_acquire_refresh_lock(dir.path(), &id(), &CoordinatorConfig::default(), 1_000, 42)
                .unwrap();
        assert_eq!(outcome, RefreshLockOutcome::Acquired);
    }

    #[test]
    fn refresh_lock_singleflights_within_window() {
        let dir = tempdir().unwrap();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 500,
        };
        // Caller A acquires at t=1000.
        try_acquire_refresh_lock(dir.path(), &id(), &cfg, 1_000, 1).unwrap();
        // Caller B arrives at t=1100 — well within 500ms window.
        let outcome = try_acquire_refresh_lock(dir.path(), &id(), &cfg, 1_100, 2).unwrap();
        match outcome {
            RefreshLockOutcome::HeldFresh { held_at_ms } => {
                assert_eq!(held_at_ms, 1_000);
            }
            other => panic!("expected HeldFresh, got {other:?}"),
        }
    }

    #[test]
    fn refresh_lock_can_be_taken_over_after_window_expires() {
        let dir = tempdir().unwrap();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 500,
        };
        try_acquire_refresh_lock(dir.path(), &id(), &cfg, 1_000, 1).unwrap();
        // Caller B arrives at t=1500 (exactly at the window — counts as expired).
        let outcome = try_acquire_refresh_lock(dir.path(), &id(), &cfg, 1_500, 2).unwrap();
        assert_eq!(outcome, RefreshLockOutcome::Acquired);
    }

    #[test]
    fn release_refresh_lock_is_idempotent() {
        let dir = tempdir().unwrap();
        let mesh = id();
        // Releasing when no lock exists is fine.
        release_refresh_lock(dir.path(), &mesh).unwrap();
        // Acquire, release, release again.
        try_acquire_refresh_lock(dir.path(), &mesh, &CoordinatorConfig::default(), 1_000, 1)
            .unwrap();
        release_refresh_lock(dir.path(), &mesh).unwrap();
        release_refresh_lock(dir.path(), &mesh).unwrap();
    }

    #[test]
    fn unsafe_chars_in_identity_dont_escape_root() {
        let dir = tempdir().unwrap();
        let mesh = MeshIdentity::new("../../etc/passwd");
        let beacon = make_beacon(1_000, vec!["general"]);
        publish(dir.path(), &mesh, &beacon).unwrap();
        // Sanitized identity stays under the accounts/ subtree.
        let root = account_root(dir.path(), &mesh);
        let canon_root = root.canonicalize().unwrap();
        let canon_home = dir.path().canonicalize().unwrap();
        assert!(canon_root.starts_with(canon_home));
    }

    #[test]
    fn refresh_lock_singleflights_under_concurrent_acquire() {
        // Race N threads at the same time against an empty lock.
        // Exactly ONE must return Acquired; the rest see HeldFresh.
        // Validates the create_new atomicity vs. the older
        // read-then-write race that PR review caught.
        use std::sync::Arc;
        use std::thread;

        const N: usize = 16;
        let dir = Arc::new(tempdir().unwrap());
        let mesh = Arc::new(id());
        let cfg = Arc::new(CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 10_000, // big window so no takeover races
        });
        let barrier = Arc::new(std::sync::Barrier::new(N));

        let handles: Vec<_> = (0..N)
            .map(|i| {
                let dir = Arc::clone(&dir);
                let mesh = Arc::clone(&mesh);
                let cfg = Arc::clone(&cfg);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    // Sync all threads so they hit the lock simultaneously.
                    barrier.wait();
                    try_acquire_refresh_lock(dir.path(), &mesh, &cfg, 1_000, i as u32).unwrap()
                })
            })
            .collect();

        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let acquired = outcomes
            .iter()
            .filter(|o| matches!(o, RefreshLockOutcome::Acquired))
            .count();
        let held_fresh = outcomes
            .iter()
            .filter(|o| matches!(o, RefreshLockOutcome::HeldFresh { .. }))
            .count();
        assert_eq!(
            acquired, 1,
            "exactly one acquire across {N} racers, got {acquired} (outcomes: {outcomes:?})"
        );
        assert_eq!(held_fresh, N - 1, "remaining racers must see HeldFresh");
    }

    #[test]
    fn refresh_lock_takeover_under_concurrent_stale() {
        // After a stale lock, race N threads. Exactly one should
        // succeed the takeover; the rest see HeldFresh on the new
        // holder's just-written timestamp. Validates the
        // remove-then-retry path under contention.
        use std::sync::Arc;
        use std::thread;

        const N: usize = 8;
        let dir = Arc::new(tempdir().unwrap());
        let mesh = Arc::new(id());
        let cfg = Arc::new(CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 100,
        });
        // Plant a stale lock (held_at_ms=0, now=10_000, window=100 → stale).
        try_acquire_refresh_lock(dir.path(), &mesh, &cfg, 0, 999).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|i| {
                let dir = Arc::clone(&dir);
                let mesh = Arc::clone(&mesh);
                let cfg = Arc::clone(&cfg);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    try_acquire_refresh_lock(dir.path(), &mesh, &cfg, 10_000, i as u32).unwrap()
                })
            })
            .collect();

        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let acquired = outcomes
            .iter()
            .filter(|o| matches!(o, RefreshLockOutcome::Acquired))
            .count();
        assert_eq!(
            acquired, 1,
            "exactly one takeover across {N} racers, got {acquired} (outcomes: {outcomes:?})"
        );
    }

    #[test]
    fn beacon_is_fresh_uses_saturating_sub_for_clock_skew() {
        let beacon = make_beacon(2_000, vec!["general"]);
        // now_ms < heartbeat_at_ms — saturating yields 0, < ttl, so fresh.
        assert!(beacon.is_fresh(1_000, 500));
    }
}
