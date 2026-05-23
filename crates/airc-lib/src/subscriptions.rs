//! Channel-subscription set — the multi-channel model the account-mesh
//! join contract requires.
//!
//! The shape is the **subscription set** — an ordered list of
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
//! ## Storage
//!
//! Persisted through `airc-store` ORM tables. There is no JSON
//! sidecar for subscription/default-channel state.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use airc_store::{EventStore, StoredSubscription};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::RoomId;

use crate::error::AircError;
use crate::room::Room;
use crate::stream::EventFilter;
use crate::Airc;

const SUBSCRIPTIONS_VERSION: u32 = 1;

/// Namespace UUID for deriving channel UUIDs from
/// `(mesh_identity, channel_name)`.
const SUBSCRIPTIONS_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xc2, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// What can go wrong loading or saving the subscription set.
#[derive(Debug)]
pub enum SubscriptionError {
    Store(airc_store::StoreError),
    Clock(std::time::SystemTimeError),
    InvalidChannelName(ChannelNameError),
}

impl std::fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "subscriptions store: {error}"),
            Self::Clock(error) => write!(f, "subscriptions clock: {error}"),
            Self::InvalidChannelName(error) => write!(f, "invalid channel name: {error}"),
        }
    }
}

impl std::error::Error for SubscriptionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::InvalidChannelName(error) => Some(error),
        }
    }
}

impl From<airc_store::StoreError> for SubscriptionError {
    fn from(value: airc_store::StoreError) -> Self {
        Self::Store(value)
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

/// A validated channel name. Normalized so `#general`, `General`,
/// and `general` all canonicalize to `general`. Display retains the
/// `#` prefix because that's how channels appear in user-facing copy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelName(String);

impl ChannelName {
    pub(crate) fn general() -> Self {
        Self("general".to_string())
    }

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

impl Serialize for MeshIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MeshIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::new(raw))
    }
}

impl Serialize for ChannelName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
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

    /// Sentinel for callers that don't yet have a real identity.
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        Self::with_wire(identity, name, wire)
    }

    /// Construct a subscription whose local-fs wire is rooted at the
    /// account-wide machine home instead of the caller's scope home.
    /// This is what makes `~/.airc`, `repo/.airc`, and other scopes on
    /// the same OS account converge on one local data plane.
    pub fn new_with_wire_root(
        wire_root: &Path,
        identity: &MeshIdentity,
        name: ChannelName,
    ) -> Result<Self, SubscriptionError> {
        let wire = wire_root.join("wires").join(name.as_str());
        Self::with_wire(identity, name, wire)
    }

    pub fn with_wire(
        identity: &MeshIdentity,
        name: ChannelName,
        wire: PathBuf,
    ) -> Result<Self, SubscriptionError> {
        let room_id = derive_room_id(identity, &name);
        Ok(Self {
            name,
            room_id,
            wire,
            joined_at_ms: now_ms()?,
        })
    }

    pub fn as_room(&self) -> Room {
        Room {
            version: 1,
            name: self.name.as_str().to_string(),
            wire: self.wire.clone(),
            channel: self.room_id,
            joined_at_ms: self.joined_at_ms,
        }
    }
}

/// All channels this scope is subscribed to, plus the default-channel
/// pointer for short-shape commands and the parted set so re-running
/// [`Airc::join_default_context`](crate::Airc::join_default_context)
/// doesn't auto-restore a channel the user left.
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Add or replace a subscription using an account-wide local wire
    /// root. See [`Subscription::new_with_wire_root`].
    pub fn subscribe_with_wire_root(
        &mut self,
        wire_root: &Path,
        identity: &MeshIdentity,
        name: ChannelName,
    ) -> Result<Subscription, SubscriptionError> {
        self.parted.remove(&name);
        if let Some(existing) = self.subscribed.get(&name) {
            return Ok(existing.clone());
        }
        let sub = Subscription::new_with_wire_root(wire_root, identity, name.clone())?;
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

/// Load the subscription set from the durable store. If no rows exist
/// yet, return an empty set; callers decide how to seed it.
pub async fn load_or_init(store: &dyn EventStore) -> Result<SubscriptionSet, SubscriptionError> {
    let mut set = SubscriptionSet::empty();
    for row in store.load_subscriptions().await? {
        let name = ChannelName::new(&row.channel_name)?;
        if row.parted {
            set.parted.insert(name);
            continue;
        }
        let subscription = Subscription {
            name: name.clone(),
            room_id: row.room_id,
            wire: PathBuf::from(row.wire),
            joined_at_ms: row.joined_at_ms,
        };
        if row.is_default {
            set.default = Some(name.clone());
        }
        set.subscribed.insert(name, subscription);
    }
    if set
        .default
        .as_ref()
        .is_some_and(|name| !set.subscribed.contains_key(name))
    {
        set.default = set.subscribed.keys().next().cloned();
    }
    Ok(set)
}

/// Save the subscription set through the durable store. This is the
/// only persistence path for subscriptions/default channel state.
pub async fn save(store: &dyn EventStore, set: &SubscriptionSet) -> Result<(), SubscriptionError> {
    let mut rows = Vec::new();
    for subscription in set.all() {
        rows.push(StoredSubscription {
            channel_name: subscription.name.as_str().to_string(),
            room_id: subscription.room_id,
            wire: subscription.wire.to_string_lossy().into_owned(),
            joined_at_ms: subscription.joined_at_ms,
            is_default: set.default.as_ref() == Some(&subscription.name),
            parted: false,
        });
    }
    for channel in &set.parted {
        if !set.subscribed.contains_key(channel) {
            rows.push(StoredSubscription {
                channel_name: channel.as_str().to_string(),
                room_id: derive_room_id(&MeshIdentity::unset(), channel),
                wire: String::new(),
                joined_at_ms: 0,
                is_default: false,
                parted: true,
            });
        }
    }
    store.replace_subscriptions(rows).await?;
    Ok(())
}

impl Airc {
    /// Load this scope's subscription set for consumer surfaces.
    pub async fn subscription_set(&self) -> Result<SubscriptionSet, AircError> {
        Ok(load_or_init(self.event_store()).await?)
    }

    /// Return all active channel subscriptions for this scope.
    ///
    /// Consumer integrations use this instead of parsing `airc status`
    /// or reading the store directly. Ordering is deterministic by
    /// channel name.
    pub async fn subscriptions(&self) -> Result<Vec<Subscription>, AircError> {
        let set = self.subscription_set().await?;
        Ok(set.all().cloned().collect())
    }

    /// True when this scope is subscribed to `channel`.
    pub async fn is_subscribed(&self, channel: &ChannelName) -> Result<bool, AircError> {
        let set = self.subscription_set().await?;
        Ok(set.subscribed.contains_key(channel))
    }

    /// Return the default room used by short-shape commands such as
    /// `airc msg "..."`.
    pub async fn default_room(&self) -> Result<Room, AircError> {
        self.current_room().await
    }

    /// Cursor of the newest event in a subscribed channel.
    ///
    /// `None` means either the channel has no events yet or this
    /// scope is not subscribed to it. Use [`Self::is_subscribed`] when
    /// callers need to distinguish those cases.
    pub async fn subscription_cursor(
        &self,
        channel: &ChannelName,
    ) -> Result<Option<airc_core::TranscriptCursor>, AircError> {
        let set = self.subscription_set().await?;
        let Some(subscription) = set.subscribed.get(channel) else {
            return Ok(None);
        };
        Ok(self
            .inner
            .store
            .latest_cursor(Some(subscription.room_id))
            .await?)
    }

    pub(crate) async fn subscribed_event_filter(
        &self,
        mut filter: EventFilter,
    ) -> Result<EventFilter, AircError> {
        if filter.channel.is_some() || !filter.channels.is_empty() {
            return Ok(filter);
        }
        filter.channels = self.subscribed_room_ids().await?;
        Ok(filter)
    }

    pub(crate) async fn ensure_subscribed_room_subscribers(&self) -> Result<(), AircError> {
        for wire in self.subscribed_wires().await? {
            self.ensure_wire_subscriber(&wire).await?;
        }
        Ok(())
    }

    async fn subscribed_room_ids(&self) -> Result<Vec<RoomId>, AircError> {
        let mut room_ids = Vec::new();
        let set = self.subscription_set().await?;
        for subscription in set.all() {
            push_unique(&mut room_ids, subscription.room_id);
        }

        if room_ids.is_empty() {
            let identity = self.mesh_identity().await?;
            push_unique(
                &mut room_ids,
                Subscription::new_with_wire_root(
                    &self.inner.wire_root,
                    &identity,
                    ChannelName::new("general").map_err(SubscriptionError::from)?,
                )?
                .room_id,
            );
        }
        Ok(room_ids)
    }

    pub(crate) async fn subscribed_wires(&self) -> Result<Vec<PathBuf>, AircError> {
        let mut wires = Vec::new();
        let set = self.subscription_set().await?;
        for subscription in set.all() {
            push_unique_path(&mut wires, subscription.wire.clone());
        }
        if wires.is_empty() {
            let identity = self.mesh_identity().await?;
            push_unique_path(
                &mut wires,
                Subscription::new_with_wire_root(
                    &self.inner.wire_root,
                    &identity,
                    ChannelName::new("general").map_err(SubscriptionError::from)?,
                )?
                .wire,
            );
        }
        Ok(wires)
    }
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn push_unique_path(items: &mut Vec<PathBuf>, item: PathBuf) {
    if !items.contains(&item) {
        items.push(item);
    }
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
    fn subscribe_with_wire_root_uses_machine_account_wire() {
        let scope_a = tempdir().unwrap();
        let scope_b = tempdir().unwrap();
        let machine_home = tempdir().unwrap();
        let id = MeshIdentity::new("joelteply");
        let channel = ChannelName::new("general").unwrap();
        let mut a = SubscriptionSet::empty();
        let mut b = SubscriptionSet::empty();

        let a_sub = a
            .subscribe_with_wire_root(machine_home.path(), &id, channel.clone())
            .unwrap();
        let b_sub = b
            .subscribe_with_wire_root(machine_home.path(), &id, channel)
            .unwrap();

        assert_eq!(a_sub.room_id, b_sub.room_id);
        assert_eq!(a_sub.wire, b_sub.wire);
        assert_eq!(
            a_sub.wire,
            machine_home.path().join("wires").join("general")
        );
        assert!(
            !a_sub.wire.starts_with(scope_a.path()) && !b_sub.wire.starts_with(scope_b.path()),
            "same-machine account mesh must not isolate local data-plane per project scope"
        );
    }

    #[test]
    fn subscribe_accepts_arbitrary_user_and_domain_channels() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();

        for name in [
            "continuum-activity-7",
            "openclaw-workspace_alpha",
            "useideem",
            "friend-general",
            "forge-lora-slots",
        ] {
            set.subscribe(home, &id, ChannelName::new(name).unwrap())
                .unwrap();
        }

        let names = set
            .channel_names()
            .map(ChannelName::as_str)
            .collect::<Vec<_>>();
        assert!(names.contains(&"continuum-activity-7"));
        assert!(names.contains(&"openclaw-workspace_alpha"));
        assert!(names.contains(&"useideem"));
        assert!(names.contains(&"friend-general"));
        assert!(names.contains(&"forge-lora-slots"));
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

    #[tokio::test]
    async fn save_load_round_trip_preserves_set() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let store = InMemoryEventStore::new();
        let id = MeshIdentity::new("joelteply");
        let mut set = SubscriptionSet::empty();
        set.subscribe(home, &id, ChannelName::new("general").unwrap())
            .unwrap();
        set.subscribe(home, &id, ChannelName::new("cambriantech").unwrap())
            .unwrap();
        save(&store, &set).await.unwrap();

        let loaded = load_or_init(&store).await.unwrap();
        assert_eq!(loaded, set);
    }

    #[tokio::test]
    async fn empty_set_when_store_has_no_rows() {
        let store = InMemoryEventStore::new();
        let set = load_or_init(&store).await.unwrap();
        assert!(set.subscribed.is_empty());
        assert!(set.default.is_none());
        assert!(set.parted.is_empty());
    }

    #[tokio::test]
    async fn airc_exposes_subscription_query_api() {
        let dir = tempdir().unwrap();
        let airc = Airc::open(dir.path()).await.unwrap();

        airc.join("general").await.unwrap();
        airc.join("cambriantech").await.unwrap();

        let subscriptions = airc.subscriptions().await.unwrap();
        let names = subscriptions
            .iter()
            .map(|subscription| subscription.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["cambriantech", "general"]);

        let cambriantech = ChannelName::new("cambriantech").unwrap();
        let general = ChannelName::new("general").unwrap();
        let missing = ChannelName::new("not-joined").unwrap();

        assert!(airc.is_subscribed(&cambriantech).await.unwrap());
        assert!(airc.is_subscribed(&general).await.unwrap());
        assert!(!airc.is_subscribed(&missing).await.unwrap());

        let default = airc.default_room().await.unwrap();
        assert_eq!(default.name, "cambriantech");

        assert!(airc
            .subscription_cursor(&cambriantech)
            .await
            .unwrap()
            .is_none());
        airc.say("cursor proof").await.unwrap();
        assert!(airc
            .subscription_cursor(&cambriantech)
            .await
            .unwrap()
            .is_some());
        assert!(airc.subscription_cursor(&general).await.unwrap().is_none());
        assert!(airc.subscription_cursor(&missing).await.unwrap().is_none());
    }
}
