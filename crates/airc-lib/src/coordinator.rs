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
//! Presence beacons live in the shared machine-account
//! `events.sqlite` store (`beacons` + `beacon_channels` tables).
//! Remote-refresh locks live in the same store (`refresh_locks`).
//! Runtime presence is never kept in JSON sidecars.
//!
//! Each scope (Claude tab, Codex tab, persona instance, daemon, etc.)
//! writes ONE row keyed by `(mesh_identity, peer_id)`. The peer-id is
//! the scope's stable identifier (from `Airc::peer_id`), so two
//! processes from the same scope coalesce; two scopes never collide.
//!
//! ## TTL and singleflight
//!
//! Two distinct concerns:
//!
//! 1. **Beacon TTL** — a beacon older than `heartbeat_ttl_ms` is
//!    considered stale. Stale beacons appear in
//!    [`CoordinatorSnapshot::stale`] separately from live ones. Stale
//!    beacons stay in the store until `drain_stale_store` runs, so a
//!    transient crash doesn't immediately purge the record (recovery
//!    still sees the old subscriptions).
//! 2. **Remote-refresh singleflight** — when a join needs the
//!    rare-and-expensive remote registry refresh (GitHub gist pull),
//!    [`try_acquire_refresh_lock`] takes the store-backed refresh row.
//!    Concurrent joins return `HeldFresh` and re-use the snapshot the
//!    lock-holder produces. Without this, ten local agents starting
//!    `airc join` simultaneously would each hammer GitHub.
//!
//! ## Scope: what's in this PR
//!
//! The coordinator API is store-first. File-backed beacon helpers
//! were removed after the SeaORM cut so production callers cannot
//! accidentally reintroduce split-brain JSON presence state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::subscriptions::{ChannelName, ChannelNameError, MeshIdentity};
use airc_core::PeerId;
use airc_store::{EventStore, StoreError, StoredBeacon, StoredRefreshLockOutcome};

const BEACON_VERSION: u32 = 1;
const ACCOUNTS_DIR: &str = "accounts";

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
    Store(StoreError),
    Channel(ChannelNameError),
}

impl std::fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "coordinator I/O: {error}"),
            Self::Json(error) => write!(f, "coordinator JSON: {error}"),
            Self::Store(error) => write!(f, "coordinator store: {error}"),
            Self::Channel(error) => write!(f, "coordinator channel: {error}"),
        }
    }
}

impl std::error::Error for CoordinatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Channel(error) => Some(error),
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

impl From<StoreError> for CoordinatorError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl From<ChannelNameError> for CoordinatorError {
    fn from(value: ChannelNameError) -> Self {
        Self::Channel(value)
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

/// Publish the caller's beacon into the durable coordinator store.
pub async fn publish_store(
    store: &dyn EventStore,
    identity: &MeshIdentity,
    beacon: &PresenceBeacon,
) -> Result<(), CoordinatorError> {
    store
        .save_beacon(to_stored_beacon(identity, beacon))
        .await?;
    Ok(())
}

/// Read the caller's own beacon, if it exists. `None` distinguishes
/// "no prior publish" from "publish exists with stale heartbeat" —
/// the caller computes freshness from the returned beacon's
/// `is_fresh`.
pub async fn load_own_beacon_store(
    store: &dyn EventStore,
    identity: &MeshIdentity,
    peer_id: PeerId,
) -> Result<Option<PresenceBeacon>, CoordinatorError> {
    store
        .load_beacon(identity.as_str(), peer_id)
        .await?
        .map(from_stored_beacon)
        .transpose()
}

/// Build a snapshot of all beacons for a mesh identity. Beacons
/// whose `heartbeat_at_ms` is within `config.heartbeat_ttl_ms` of
/// `now_ms` land in `live`; the rest in `stale`.
pub async fn snapshot_store(
    store: &dyn EventStore,
    identity: &MeshIdentity,
    config: &CoordinatorConfig,
    now_ms: u64,
) -> Result<CoordinatorSnapshot, CoordinatorError> {
    let mut live = Vec::new();
    let mut stale = Vec::new();
    for beacon in store.list_beacons(identity.as_str()).await? {
        let beacon = from_stored_beacon(beacon)?;
        if beacon.is_fresh(now_ms, config.heartbeat_ttl_ms) {
            live.push(beacon);
        } else {
            stale.push(beacon);
        }
    }
    live.sort_by_key(|b| b.peer_id.to_string());
    stale.sort_by_key(|b| b.peer_id.to_string());
    let live_channels = unique_channels_in(&live);
    Ok(CoordinatorSnapshot {
        mesh_identity: identity.clone(),
        root: PathBuf::from("airc-store://beacons"),
        live,
        stale,
        live_channels,
        fetched_at_ms: now_ms,
    })
}

/// Delete store rows for all entries currently in
/// [`CoordinatorSnapshot::stale`]. Best-effort — missing rows
/// (raced with another draining process) aren't an error. Returns
/// the count of beacons removed.
///
/// Separate from `snapshot_store` so callers opt in to destructive
/// action.
pub async fn drain_stale_store(
    store: &dyn EventStore,
    identity: &MeshIdentity,
    snapshot: &CoordinatorSnapshot,
) -> Result<usize, CoordinatorError> {
    let peer_ids = snapshot
        .stale
        .iter()
        .map(|beacon| beacon.peer_id)
        .collect::<Vec<_>>();
    Ok(store.delete_beacons(identity.as_str(), &peer_ids).await?)
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

/// Try to acquire the remote-refresh lock through the durable store.
/// Singleflight pattern: only one caller at a time should hammer the
/// remote registry (GitHub gist), so subsequent callers within
/// `refresh_interval_ms` see `HeldFresh` and re-use the lock-holder's
/// snapshot.
///
/// Atomicity is owned by the store. SQLite uses a primary-key insert
/// followed by compare-and-set stale takeover on the `refresh_locks`
/// row; there are no coordinator lock files and no JSON sidecars.
pub async fn try_acquire_refresh_lock(
    store: &dyn EventStore,
    identity: &MeshIdentity,
    config: &CoordinatorConfig,
    now_ms: u64,
    holder_pid: u32,
) -> Result<RefreshLockOutcome, CoordinatorError> {
    let outcome = store
        .try_acquire_refresh_lock(
            identity.as_str(),
            now_ms,
            config.refresh_interval_ms,
            holder_pid,
        )
        .await?;
    Ok(match outcome {
        StoredRefreshLockOutcome::Acquired => RefreshLockOutcome::Acquired,
        StoredRefreshLockOutcome::HeldFresh { held_at_ms } => {
            RefreshLockOutcome::HeldFresh { held_at_ms }
        }
    })
}

/// Release the refresh lock. Idempotent: a missing row is not an
/// error (e.g., concurrent drain, or another process already took
/// over after our holder window expired).
pub async fn release_refresh_lock(
    store: &dyn EventStore,
    identity: &MeshIdentity,
) -> Result<(), CoordinatorError> {
    store.release_refresh_lock(identity.as_str()).await?;
    Ok(())
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

fn to_stored_beacon(identity: &MeshIdentity, beacon: &PresenceBeacon) -> StoredBeacon {
    StoredBeacon {
        mesh_identity: identity.as_str().to_string(),
        peer_id: beacon.peer_id,
        scope_home: beacon.scope_home.to_string_lossy().to_string(),
        subscribed_channels: beacon
            .subscribed_channels
            .iter()
            .map(|channel| channel.as_str().to_string())
            .collect(),
        pid: beacon.pid,
        published_at_ms: beacon.published_at_ms,
        heartbeat_at_ms: beacon.heartbeat_at_ms,
    }
}

fn from_stored_beacon(beacon: StoredBeacon) -> Result<PresenceBeacon, CoordinatorError> {
    let subscribed_channels = beacon
        .subscribed_channels
        .into_iter()
        .map(ChannelName::new)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PresenceBeacon {
        version: BEACON_VERSION,
        peer_id: beacon.peer_id,
        scope_home: PathBuf::from(beacon.scope_home),
        subscribed_channels,
        pid: beacon.pid,
        published_at_ms: beacon.published_at_ms,
        heartbeat_at_ms: beacon.heartbeat_at_ms,
    })
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

    #[tokio::test]
    async fn store_publish_snapshot_and_drain_round_trip() {
        let store = airc_store::InMemoryEventStore::new();
        let mesh = id();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 100,
        };
        let fresh = make_beacon(950, vec!["general", "cambriantech"]);
        let stale = make_beacon(0, vec!["ideem"]);

        publish_store(&store, &mesh, &fresh).await.unwrap();
        publish_store(&store, &mesh, &stale).await.unwrap();

        let loaded = load_own_beacon_store(&store, &mesh, fresh.peer_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, fresh);

        let snapshot = snapshot_store(&store, &mesh, &cfg, 1_000).await.unwrap();
        assert_eq!(snapshot.live.len(), 1);
        assert_eq!(snapshot.stale.len(), 1);
        assert_eq!(
            snapshot
                .live_channels
                .iter()
                .map(ChannelName::as_str)
                .collect::<Vec<_>>(),
            vec!["cambriantech", "general"]
        );

        assert_eq!(
            drain_stale_store(&store, &mesh, &snapshot).await.unwrap(),
            1
        );
        let after = snapshot_store(&store, &mesh, &cfg, 1_000).await.unwrap();
        assert!(after.stale.is_empty());
        assert_eq!(after.live.len(), 1);
    }

    #[tokio::test]
    async fn load_own_beacon_returns_none_when_absent() {
        let store = airc_store::InMemoryEventStore::new();
        let loaded = load_own_beacon_store(&store, &id(), peer()).await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn snapshot_partitions_live_and_stale_by_ttl() {
        let store = airc_store::InMemoryEventStore::new();
        let mesh = id();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 100,
        };

        let fresh = make_beacon(950, vec!["general"]);
        let stale = make_beacon(0, vec!["cambriantech"]);
        publish_store(&store, &mesh, &fresh).await.unwrap();
        publish_store(&store, &mesh, &stale).await.unwrap();

        let snap = snapshot_store(&store, &mesh, &cfg, 1_000).await.unwrap();
        assert_eq!(snap.live.len(), 1, "fresh beacon should be live");
        assert_eq!(snap.stale.len(), 1, "old beacon should be stale");
        assert_eq!(snap.live[0].peer_id, fresh.peer_id);
        assert_eq!(snap.stale[0].peer_id, stale.peer_id);
    }

    #[tokio::test]
    async fn snapshot_aggregates_live_channels_deduplicated() {
        let store = airc_store::InMemoryEventStore::new();
        let mesh = id();
        let cfg = CoordinatorConfig::default();

        let a = make_beacon(1_000, vec!["general", "cambriantech"]);
        let b = make_beacon(1_000, vec!["general", "ideem"]);
        publish_store(&store, &mesh, &a).await.unwrap();
        publish_store(&store, &mesh, &b).await.unwrap();

        let snap = snapshot_store(&store, &mesh, &cfg, 1_000).await.unwrap();
        let names: Vec<&str> = snap.live_channels.iter().map(ChannelName::as_str).collect();
        assert_eq!(names, vec!["cambriantech", "general", "ideem"]);
    }

    #[tokio::test]
    async fn snapshot_empty_when_no_beacons() {
        let store = airc_store::InMemoryEventStore::new();
        let snap = snapshot_store(&store, &id(), &CoordinatorConfig::default(), 0)
            .await
            .unwrap();
        assert!(snap.live.is_empty());
        assert!(snap.stale.is_empty());
        assert!(snap.live_channels.is_empty());
    }

    #[tokio::test]
    async fn snapshot_isolates_identities() {
        let store = airc_store::InMemoryEventStore::new();
        let cfg = CoordinatorConfig::default();
        let mine = make_beacon(1_000, vec!["general"]);
        let theirs = make_beacon(1_000, vec!["general"]);
        publish_store(&store, &id(), &mine).await.unwrap();
        publish_store(&store, &other_id(), &theirs).await.unwrap();

        let my_snap = snapshot_store(&store, &id(), &cfg, 1_000).await.unwrap();
        let their_snap = snapshot_store(&store, &other_id(), &cfg, 1_000)
            .await
            .unwrap();
        assert_eq!(my_snap.live.len(), 1);
        assert_eq!(their_snap.live.len(), 1);
        assert_eq!(my_snap.live[0].peer_id, mine.peer_id);
        assert_eq!(their_snap.live[0].peer_id, theirs.peer_id);
    }

    #[tokio::test]
    async fn drain_stale_removes_only_stale_rows() {
        let store = airc_store::InMemoryEventStore::new();
        let mesh = id();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 100,
        };
        let fresh = make_beacon(950, vec!["general"]);
        let stale = make_beacon(0, vec!["general"]);
        publish_store(&store, &mesh, &fresh).await.unwrap();
        publish_store(&store, &mesh, &stale).await.unwrap();

        let snap = snapshot_store(&store, &mesh, &cfg, 1_000).await.unwrap();
        let removed = drain_stale_store(&store, &mesh, &snap).await.unwrap();
        assert_eq!(removed, 1);

        // Re-snapshot: only the fresh beacon should remain.
        let after = snapshot_store(&store, &mesh, &cfg, 1_000).await.unwrap();
        assert_eq!(after.live.len(), 1);
        assert_eq!(after.stale.len(), 0);
    }

    #[tokio::test]
    async fn publish_is_idempotent_via_store_upsert() {
        let store = airc_store::InMemoryEventStore::new();
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
        publish_store(&store, &mesh, &first).await.unwrap();
        publish_store(&store, &mesh, &second).await.unwrap();
        let loaded = load_own_beacon_store(&store, &mesh, peer_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.heartbeat_at_ms, 2_000, "second publish wins");
    }

    #[tokio::test]
    async fn refresh_lock_first_caller_acquires() {
        let store = airc_store::InMemoryEventStore::new();
        let outcome =
            try_acquire_refresh_lock(&store, &id(), &CoordinatorConfig::default(), 1_000, 42)
                .await
                .unwrap();
        assert_eq!(outcome, RefreshLockOutcome::Acquired);
    }

    #[tokio::test]
    async fn refresh_lock_singleflights_within_window() {
        let store = airc_store::InMemoryEventStore::new();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 500,
        };
        // Caller A acquires at t=1000.
        try_acquire_refresh_lock(&store, &id(), &cfg, 1_000, 1)
            .await
            .unwrap();
        // Caller B arrives at t=1100 — well within 500ms window.
        let outcome = try_acquire_refresh_lock(&store, &id(), &cfg, 1_100, 2)
            .await
            .unwrap();
        match outcome {
            RefreshLockOutcome::HeldFresh { held_at_ms } => {
                assert_eq!(held_at_ms, 1_000);
            }
            other => panic!("expected HeldFresh, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_lock_can_be_taken_over_after_window_expires() {
        let store = airc_store::InMemoryEventStore::new();
        let cfg = CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 500,
        };
        try_acquire_refresh_lock(&store, &id(), &cfg, 1_000, 1)
            .await
            .unwrap();
        // Caller B arrives at t=1500 (exactly at the window — counts as expired).
        let outcome = try_acquire_refresh_lock(&store, &id(), &cfg, 1_500, 2)
            .await
            .unwrap();
        assert_eq!(outcome, RefreshLockOutcome::Acquired);
    }

    #[tokio::test]
    async fn release_refresh_lock_is_idempotent() {
        let store = airc_store::InMemoryEventStore::new();
        let mesh = id();
        release_refresh_lock(&store, &mesh).await.unwrap();
        try_acquire_refresh_lock(&store, &mesh, &CoordinatorConfig::default(), 1_000, 1)
            .await
            .unwrap();
        release_refresh_lock(&store, &mesh).await.unwrap();
        release_refresh_lock(&store, &mesh).await.unwrap();
    }

    #[test]
    fn unsafe_chars_in_identity_dont_escape_root() {
        let dir = tempdir().unwrap();
        let mesh = MeshIdentity::new("../../etc/passwd");
        // Sanitized identity stays under the accounts/ subtree.
        let root = account_root(dir.path(), &mesh);
        assert!(root.starts_with(dir.path()));
        assert_eq!(
            root.file_name().unwrap().to_string_lossy(),
            "..-..-etc-passwd"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn refresh_lock_singleflights_under_concurrent_acquire() {
        // Race N tasks at the same time against an empty store row.
        // Exactly ONE must return Acquired; the rest see HeldFresh.
        const N: usize = 16;
        let store = std::sync::Arc::new(airc_store::SqliteEventStore::in_memory().await.unwrap());
        let mesh = std::sync::Arc::new(id());
        let cfg = std::sync::Arc::new(CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 10_000, // big window so no takeover races
        });
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(N));

        let handles: Vec<_> = (0..N)
            .map(|i| {
                let store = std::sync::Arc::clone(&store);
                let mesh = std::sync::Arc::clone(&mesh);
                let cfg = std::sync::Arc::clone(&cfg);
                let barrier = std::sync::Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    try_acquire_refresh_lock(&*store, &mesh, &cfg, 1_000, i as u32)
                        .await
                        .unwrap()
                })
            })
            .collect();

        let mut outcomes = Vec::with_capacity(N);
        for handle in handles {
            outcomes.push(handle.await.unwrap());
        }
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn refresh_lock_takeover_under_concurrent_stale() {
        // After a stale lock, race N tasks. Exactly one should
        // succeed the compare-and-set takeover.
        const N: usize = 8;
        let store = std::sync::Arc::new(airc_store::SqliteEventStore::in_memory().await.unwrap());
        let mesh = std::sync::Arc::new(id());
        let cfg = std::sync::Arc::new(CoordinatorConfig {
            heartbeat_ttl_ms: 1_000,
            refresh_interval_ms: 100,
        });
        // Plant a stale lock (held_at_ms=0, now=10_000, window=100 → stale).
        try_acquire_refresh_lock(&*store, &mesh, &cfg, 0, 999)
            .await
            .unwrap();

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|i| {
                let store = std::sync::Arc::clone(&store);
                let mesh = std::sync::Arc::clone(&mesh);
                let cfg = std::sync::Arc::clone(&cfg);
                let barrier = std::sync::Arc::clone(&barrier);
                tokio::spawn(async move {
                    barrier.wait().await;
                    try_acquire_refresh_lock(&*store, &mesh, &cfg, 10_000, i as u32)
                        .await
                        .unwrap()
                })
            })
            .collect();

        let mut outcomes = Vec::with_capacity(N);
        for handle in handles {
            outcomes.push(handle.await.unwrap());
        }
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
