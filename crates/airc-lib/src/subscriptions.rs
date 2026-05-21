//! Channel-subscription set — the multi-channel model the account-mesh
//! join contract requires.
//!
//! Replaces (atop, not yet under) the single-room
//! [`crate::room::Room`] model. The old shape persisted exactly one
//! `(name, RoomId, wire)` in `<home>/room.json`; switching channels
//! meant overwriting it, so a scope could only ever see one channel's
//! traffic. That's wrong for monitors and hooks: a scope subscribed to
//! `#general` AND `#cambriantech` needs to surface events from both
//! simultaneously.
//!
//! The new shape is the **subscription set** — an ordered list of
//! channels this scope is subscribed to, plus a "default" pointer for
//! short-shape commands (`airc msg "hi"`) and a "parted" set so we
//! don't auto-resubscribe to a channel the user explicitly left when
//! [`Airc::join_default_context`](crate::Airc::join_default_context)
//! re-infers context.
//!
//! ## RoomId derivation
//!
//! Each subscribed channel is a typed [`ChannelName`] with a
//! deterministic [`RoomId`] — see [`derive_room_id`]. The derivation
//! is namespaced by an opaque [`MeshIdentity`] string (intended to be
//! the user's authenticated Git/GitHub identity) so that two scopes on
//! the same identity converge to the same `RoomId` for a given
//! channel, and cross-identity channels do NOT collide. For v1, the
//! identity is supplied by the caller; a follow-up wires it to
//! `gh api user --jq .login` via the machine-global coordinator.
//!
//! Defaulting `identity = ""` reproduces the pre-existing name-only
//! derivation in `room::Room::from_name` for back-compat during the
//! migration window.
//!
//! ## Storage
//!
//! Persisted to `<home>/subscriptions.json`. Schema version 1. On
//! first load, if `subscriptions.json` is absent but `room.json`
//! exists, the legacy single-room file is converted to a one-entry
//! `SubscriptionSet` and saved alongside it (the room file stays for
//! callers still using [`crate::Airc::current_room`] until the next
//! slice removes them).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::RoomId;

use crate::room::{self, Room, RoomError};

const SUBSCRIPTIONS_FILENAME: &str = "subscriptions.json";
const SUBSCRIPTIONS_VERSION: u32 = 1;

/// Namespace UUID for deriving channel UUIDs from
/// `(mesh_identity, channel_name)`. Distinct from `room::ROOM_NAMESPACE`
/// (which was name-only) so the new derivation can coexist with legacy
/// rooms during migration without ambiguous collisions.
const SUBSCRIPTIONS_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xc2, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// What can go wrong loading or saving the subscription set.
#[derive(Debug)]
pub enum SubscriptionError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Clock(std::time::SystemTimeError),
    SchemaVersionMismatch { found: u32, expected: u32 },
    InvalidChannelName(ChannelNameError),
    Room(RoomError),
}

impl std::fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "subscriptions I/O: {error}"),
            Self::Json(error) => write!(f, "subscriptions JSON: {error}"),
            Self::Clock(error) => write!(f, "subscriptions clock: {error}"),
            Self::SchemaVersionMismatch { found, expected } => {
                write!(f, "subscriptions.json version {found}, expected {expected}")
            }
            Self::InvalidChannelName(error) => write!(f, "invalid channel name: {error}"),
            Self::Room(error) => write!(f, "subscriptions room migration: {error}"),
        }
    }
}

impl std::error::Error for SubscriptionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::SchemaVersionMismatch { .. } => None,
            Self::InvalidChannelName(error) => Some(error),
            Self::Room(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for SubscriptionError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for SubscriptionError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::time::SystemTimeError> for SubscriptionError {
    fn from(value: std::time::SystemTimeError) -> Self {
        Self::Clock(value)
    }
}

impl From<ChannelNameError> for SubscriptionError {
    fn from(value: ChannelNameError) -> Self {
        Self::InvalidChannelName(value)
    }
}

impl From<RoomError> for SubscriptionError {
    fn from(value: RoomError) -> Self {
        Self::Room(value)
    }
}

/// A validated channel name. Normalized so `#general`, `General`,
/// and `general` all canonicalize to `general`. Display retains the
/// `#` prefix because that's how channels appear in user-facing copy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelName(String);

impl ChannelName {
    /// Construct from any user-supplied string. Strips a leading `#`
    /// if present, trims whitespace, lower-cases ASCII, and rejects
    /// anything that wouldn't be safe as both a path component and a
    /// chat label.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ChannelNameError> {
        let raw = value.as_ref().trim().trim_start_matches('#').trim();
        if raw.is_empty() {
            return Err(ChannelNameError::Empty);
        }
        let normalized: String = raw
            .chars()
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>();
        for c in normalized.chars() {
            if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(ChannelNameError::InvalidChar(c));
            }
        }
        Ok(Self(normalized))
    }

    /// Underlying normalized name with no `#` prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// User-visible form, e.g. `#general`.
    pub fn display_with_hash(&self) -> String {
        format!("#{}", self.0)
    }
}

impl std::fmt::Display for ChannelName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display_with_hash())
    }
}

impl Serialize for ChannelName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize without `#` so on-disk JSON is the normalized form.
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ChannelName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Why a [`ChannelName`] failed to parse. Closed set so callers can
/// produce specific error messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelNameError {
    #[error("channel name cannot be empty")]
    Empty,
    #[error("channel name contains invalid character '{0}' (allowed: a-z 0-9 - _)")]
    InvalidChar(char),
}

/// Opaque mesh-identity string. v1 callers may pass any stable token;
/// future revisions wire this to `gh api user --jq .login` via the
/// machine-global coordinator so all scopes on one GitHub account
/// converge to the same `RoomId` for a given channel.
///
/// Wrapper instead of bare String so misuse like
/// `derive_room_id(name, name)` is impossible.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MeshIdentity(String);

impl MeshIdentity {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Sentinel for callers that don't yet have a real identity (e.g.,
    /// unit tests, the migration path during the transition window).
    /// Returns a deterministic empty identity.
    pub fn unset() -> Self {
        Self(String::new())
    }
}

/// Derive a `RoomId` from `(mesh_identity, channel_name)`. Same inputs
/// produce the same `RoomId` on any machine, so two scopes on the same
/// identity that both subscribe to `#general` see the same room.
pub fn derive_room_id(identity: &MeshIdentity, channel: &ChannelName) -> RoomId {
    let mut bytes = Vec::with_capacity(identity.as_str().len() + 1 + channel.as_str().len());
    bytes.extend_from_slice(identity.as_str().as_bytes());
    // NUL separator so e.g. ("alice", "bob-room") and ("alice-bob", "room")
    // can never collide.
    bytes.push(0);
    bytes.extend_from_slice(channel.as_str().as_bytes());
    RoomId::from_uuid(Uuid::new_v5(&SUBSCRIPTIONS_NAMESPACE, &bytes))
}

/// One channel this scope is subscribed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub name: ChannelName,
    pub room_id: RoomId,
    pub wire: PathBuf,
    pub joined_at_ms: u64,
}

impl Subscription {
    /// Construct a subscription. Derives the `RoomId` from
    /// `(identity, name)` and the wire path from
    /// `<home>/wires/<name>/`.
    pub fn new(
        home: &Path,
        identity: &MeshIdentity,
        name: ChannelName,
    ) -> Result<Self, SubscriptionError> {
        let wire = home.join("wires").join(name.as_str());
        let room_id = derive_room_id(identity, &name);
        Ok(Self {
            name,
            room_id,
            wire,
            joined_at_ms: now_ms()?,
        })
    }
}

/// All channels this scope is subscribed to, plus the default-channel
/// pointer for short-shape commands and the parted set so re-running
/// [`Airc::join_default_context`](crate::Airc::join_default_context)
/// doesn't auto-restore a channel the user left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionSet {
    pub version: u32,
    pub subscribed: BTreeMap<ChannelName, Subscription>,
    /// Default channel for `airc msg "..."` etc. `None` means no
    /// subscriptions yet (fresh-init scope).
    pub default: Option<ChannelName>,
    /// Channels the user explicitly parted. Never auto-rejoined by
    /// `join_default_context`. Re-subscribing via `subscribe` clears
    /// the entry from this set.
    pub parted: BTreeSet<ChannelName>,
}

impl SubscriptionSet {
    /// An empty set. Used when initializing a fresh scope.
    pub fn empty() -> Self {
        Self {
            version: SUBSCRIPTIONS_VERSION,
            subscribed: BTreeMap::new(),
            default: None,
            parted: BTreeSet::new(),
        }
    }

    /// Add or replace a subscription for `name`. Idempotent: if the
    /// channel is already subscribed, the existing
    /// `joined_at_ms` is preserved (re-subscribe is a no-op for
    /// observers). Clears the channel from `parted` if present so a
    /// later default-context re-infer will keep it.
    ///
    /// Sets the channel as `default` only if no default exists yet.
    /// Explicit promotion is via [`Self::set_default`].
    pub fn subscribe(
        &mut self,
        home: &Path,
        identity: &MeshIdentity,
        name: ChannelName,
    ) -> Result<Subscription, SubscriptionError> {
        self.parted.remove(&name);
        if let Some(existing) = self.subscribed.get(&name) {
            return Ok(existing.clone());
        }
        let sub = Subscription::new(home, identity, name.clone())?;
        self.subscribed.insert(name.clone(), sub.clone());
        if self.default.is_none() {
            self.default = Some(name);
        }
        Ok(sub)
    }

    /// Remove a subscription and mark it parted so it's not
    /// auto-restored. If the removed channel was the default, the
    /// default falls back to any remaining subscription
    /// (deterministically the lowest-sorted name) or `None`.
    pub fn unsubscribe(&mut self, name: &ChannelName) -> Option<Subscription> {
        let removed = self.subscribed.remove(name);
        if removed.is_some() {
            self.parted.insert(name.clone());
            if self.default.as_ref() == Some(name) {
                self.default = self.subscribed.keys().next().cloned();
            }
        }
        removed
    }

    /// Set the default channel. Only succeeds if the channel is
    /// already subscribed; setting a non-subscribed channel as
    /// default would lie about what `airc msg` will hit.
    pub fn set_default(&mut self, name: ChannelName) -> Result<(), SubscriptionError> {
        if !self.subscribed.contains_key(&name) {
            return Err(SubscriptionError::InvalidChannelName(
                ChannelNameError::Empty,
            ));
        }
        self.default = Some(name);
        Ok(())
    }

    /// The default subscription for short-shape commands, if any.
    pub fn default_subscription(&self) -> Option<&Subscription> {
        self.default
            .as_ref()
            .and_then(|name| self.subscribed.get(name))
    }

    /// All subscriptions, sorted by name (deterministic ordering for
    /// monitor/hook iteration so the user's experience is stable).
    pub fn all(&self) -> impl Iterator<Item = &Subscription> {
        self.subscribed.values()
    }

    /// Just the names of subscribed channels — what Codex's
    /// consumer-surface PR reads to know which RoomIds to drain.
    pub fn channel_names(&self) -> impl Iterator<Item = &ChannelName> {
        self.subscribed.keys()
    }
}

/// On-disk path for the subscription set.
pub fn path_in(home: &Path) -> PathBuf {
    home.join(SUBSCRIPTIONS_FILENAME)
}

/// Load the subscription set. If `subscriptions.json` is missing but
/// `room.json` exists, migrate the single legacy room into a
/// one-entry subscription set and persist. If neither exists, return
/// an empty set (not persisted — caller decides whether to seed via
/// `join_default_context`).
pub fn load_or_init(home: &Path) -> Result<SubscriptionSet, SubscriptionError> {
    let path = path_in(home);
    if path.exists() {
        let text = std::fs::read_to_string(&path)?;
        let set: SubscriptionSet = serde_json::from_str(&text)?;
        if set.version != SUBSCRIPTIONS_VERSION {
            return Err(SubscriptionError::SchemaVersionMismatch {
                found: set.version,
                expected: SUBSCRIPTIONS_VERSION,
            });
        }
        return Ok(set);
    }

    // Migration path: legacy room.json present, no subscriptions.json.
    let room_path = room::path_in(home);
    if room_path.exists() {
        let legacy = room::load_or_default(home)?;
        let set = from_legacy_room(&legacy)?;
        save(home, &set)?;
        return Ok(set);
    }

    Ok(SubscriptionSet::empty())
}

/// Save the subscription set to disk. Always writes the canonical
/// shape; round-trip-safe (load_or_init → save → load_or_init yields
/// the same set).
pub fn save(home: &Path, set: &SubscriptionSet) -> Result<(), SubscriptionError> {
    std::fs::create_dir_all(home)?;
    let path = path_in(home);
    let text = serde_json::to_string_pretty(set)?;
    std::fs::write(&path, text)?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

/// Convert a legacy `Room` into a single-entry `SubscriptionSet`.
/// The legacy room is preserved verbatim (same wire path and
/// `joined_at_ms`); only the `RoomId` is re-derived via the new
/// `(MeshIdentity::unset(), name)` derivation so the migrated entry
/// is observable by the new derivation path. Old name-only `RoomId`s
/// from `Room::from_name` won't equal the new ones — which is the
/// point: the new derivation namespaces by identity, so legacy single-
/// room scopes get a fresh per-identity `RoomId` on migration. The
/// transition window's existing in-flight events still live under the
/// legacy `RoomId`; new sends route to the new one. Joel acknowledged
/// this discontinuity is acceptable — the rewrite is a clean break.
pub fn from_legacy_room(legacy: &Room) -> Result<SubscriptionSet, SubscriptionError> {
    let name = ChannelName::new(&legacy.name)?;
    let identity = MeshIdentity::unset();
    let room_id = derive_room_id(&identity, &name);
    let sub = Subscription {
        name: name.clone(),
        room_id,
        wire: legacy.wire.clone(),
        joined_at_ms: legacy.joined_at_ms,
    };
    let mut set = SubscriptionSet::empty();
    set.subscribed.insert(name.clone(), sub);
    set.default = Some(name);
    Ok(set)
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

    #[test]
    fn channel_name_normalizes() {
        assert_eq!(ChannelName::new("#general").unwrap().as_str(), "general");
        assert_eq!(ChannelName::new("General").unwrap().as_str(), "general");
        assert_eq!(ChannelName::new("  general  ").unwrap().as_str(), "general");
        assert_eq!(ChannelName::new("#General").unwrap().as_str(), "general");
        assert_eq!(
            ChannelName::new("cambriantech").unwrap().as_str(),
            "cambriantech"
        );
        assert_eq!(ChannelName::new("ci-bot").unwrap().as_str(), "ci-bot");
        assert_eq!(ChannelName::new("ci_bot").unwrap().as_str(), "ci_bot");
    }

    #[test]
    fn channel_name_rejects_invalid() {
        assert_eq!(ChannelName::new("").unwrap_err(), ChannelNameError::Empty);
        assert_eq!(
            ChannelName::new("   ").unwrap_err(),
            ChannelNameError::Empty
        );
        assert_eq!(ChannelName::new("#").unwrap_err(), ChannelNameError::Empty);
        assert!(matches!(
            ChannelName::new("foo bar").unwrap_err(),
            ChannelNameError::InvalidChar(' ')
        ));
        assert!(matches!(
            ChannelName::new("foo/bar").unwrap_err(),
            ChannelNameError::InvalidChar('/')
        ));
    }

    #[test]
    fn channel_name_display_keeps_hash() {
        let c = ChannelName::new("general").unwrap();
        assert_eq!(c.to_string(), "#general");
        assert_eq!(c.display_with_hash(), "#general");
    }

    #[test]
    fn channel_name_serde_round_trip() {
        let original = ChannelName::new("#general").unwrap();
        let json = serde_json::to_string(&original).unwrap();
        // Stored without `#` to keep the normalized form on disk.
        assert_eq!(json, "\"general\"");
        let back: ChannelName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn derive_room_id_is_deterministic_per_identity_channel() {
        let id = MeshIdentity::new("joelteply");
        let c = ChannelName::new("general").unwrap();
        let a = derive_room_id(&id, &c);
        let b = derive_room_id(&id, &c);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_room_id_differs_across_identities() {
        let c = ChannelName::new("general").unwrap();
        let a = derive_room_id(&MeshIdentity::new("joelteply"), &c);
        let b = derive_room_id(&MeshIdentity::new("someone-else"), &c);
        assert_ne!(
            a, b,
            "two users' #general channels MUST be different RoomIds — \
             same-name cross-user bridging would be a privacy bug"
        );
    }

    #[test]
    fn derive_room_id_differs_across_channels() {
        let id = MeshIdentity::new("joelteply");
        let a = derive_room_id(&id, &ChannelName::new("general").unwrap());
        let b = derive_room_id(&id, &ChannelName::new("cambriantech").unwrap());
        assert_ne!(a, b);
    }

    #[test]
    fn derive_room_id_nul_separator_prevents_collisions() {
        // Without the NUL separator, ("alice", "bob-c") and
        // ("alice-bob", "c") would both produce the input "alicebob-c"
        // (or "alicebobc") and collide.
        let a = derive_room_id(
            &MeshIdentity::new("alice"),
            &ChannelName::new("bob-c").unwrap(),
        );
        let b = derive_room_id(
            &MeshIdentity::new("alice-bob"),
            &ChannelName::new("c").unwrap(),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn subscribe_is_idempotent_and_seeds_default() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();
        assert!(set.default.is_none());

        let sub1 = set
            .subscribe(home, &id, ChannelName::new("general").unwrap())
            .unwrap();
        assert_eq!(set.default.as_ref().unwrap().as_str(), "general");
        assert_eq!(set.subscribed.len(), 1);

        // Idempotent: second subscribe returns the same entry.
        let sub2 = set
            .subscribe(home, &id, ChannelName::new("general").unwrap())
            .unwrap();
        assert_eq!(sub1, sub2);
        assert_eq!(set.subscribed.len(), 1);
        assert_eq!(set.default.as_ref().unwrap().as_str(), "general");
    }

    #[test]
    fn subscribe_adds_without_changing_default() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();
        set.subscribe(home, &id, ChannelName::new("general").unwrap())
            .unwrap();
        set.subscribe(home, &id, ChannelName::new("cambriantech").unwrap())
            .unwrap();
        // First subscription stays as default.
        assert_eq!(set.default.as_ref().unwrap().as_str(), "general");
        assert_eq!(set.subscribed.len(), 2);
    }

    #[test]
    fn unsubscribe_marks_parted_and_falls_back_default() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();
        let general = ChannelName::new("general").unwrap();
        let cambriantech = ChannelName::new("cambriantech").unwrap();
        set.subscribe(home, &id, general.clone()).unwrap();
        set.subscribe(home, &id, cambriantech.clone()).unwrap();

        let removed = set.unsubscribe(&general).expect("general was subscribed");
        assert_eq!(removed.name, general);
        assert!(set.parted.contains(&general));
        // Default falls back to remaining (cambriantech).
        assert_eq!(set.default, Some(cambriantech.clone()));
        assert_eq!(set.subscribed.len(), 1);
    }

    #[test]
    fn unsubscribe_then_resubscribe_clears_parted() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();
        let general = ChannelName::new("general").unwrap();
        set.subscribe(home, &id, general.clone()).unwrap();
        set.unsubscribe(&general);
        assert!(set.parted.contains(&general));

        set.subscribe(home, &id, general.clone()).unwrap();
        assert!(!set.parted.contains(&general));
        assert_eq!(set.default, Some(general));
    }

    #[test]
    fn set_default_requires_subscription() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();
        set.subscribe(home, &id, ChannelName::new("general").unwrap())
            .unwrap();
        // Setting a non-subscribed channel as default must error.
        let result = set.set_default(ChannelName::new("nowhere").unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn save_load_round_trip_preserves_set() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();
        set.subscribe(home, &id, ChannelName::new("general").unwrap())
            .unwrap();
        set.subscribe(home, &id, ChannelName::new("cambriantech").unwrap())
            .unwrap();
        save(home, &set).unwrap();

        let loaded = load_or_init(home).unwrap();
        assert_eq!(loaded, set);
    }

    #[test]
    fn migrate_from_legacy_room_when_subscriptions_absent() {
        let dir = tempdir().unwrap();
        let home = dir.path();

        // Seed the legacy room.json directly.
        let legacy = Room::from_name(home, "default").unwrap();
        room::save(home, &legacy).unwrap();
        assert!(room::path_in(home).exists());
        assert!(!path_in(home).exists());

        let set = load_or_init(home).unwrap();
        assert!(path_in(home).exists(), "migration must persist the set");
        assert_eq!(set.subscribed.len(), 1);
        let migrated = set.subscribed.values().next().unwrap();
        assert_eq!(migrated.name.as_str(), "default");
        assert_eq!(migrated.wire, legacy.wire);
        assert_eq!(migrated.joined_at_ms, legacy.joined_at_ms);
        assert_eq!(set.default.as_ref().unwrap().as_str(), "default");
    }

    #[test]
    fn empty_set_when_neither_file_exists() {
        let dir = tempdir().unwrap();
        let set = load_or_init(dir.path()).unwrap();
        assert!(set.subscribed.is_empty());
        assert!(set.default.is_none());
        assert!(set.parted.is_empty());
    }
}
