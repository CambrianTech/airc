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
//! ## The room id IS the key
//!
//! Membership is keyed by [`RoomId`] — a v4 uuid minted by airc when
//! the room is created, read-only to every caller. A [`ChannelName`]
//! rides along as a display label and addresses nothing: two rooms may
//! share a label, a room may have none, and renaming one moves nothing.
//!
//! ## Storage
//!
//! Persisted through `airc-store` ORM tables. There is no JSON
//! sidecar for subscription/default-channel state.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use airc_store::{EventStore, StoredSubscription};
use serde::{Deserialize, Serialize};

use airc_core::RoomId;

use crate::error::AircError;
use crate::room::Room;
use crate::stream::EventFilter;
use crate::Airc;

const SUBSCRIPTIONS_VERSION: u32 = 1;

/// What can go wrong loading or saving the subscription set.
#[derive(Debug)]
pub enum SubscriptionError {
    Store(airc_store::StoreError),
    Clock(std::time::SystemTimeError),
    InvalidChannelName(ChannelNameError),
    /// A room id was passed where membership is required, and this
    /// scope is not subscribed to it. The id is reported verbatim —
    /// there is no name to guess at.
    UnknownRoom(RoomId),
}

impl std::fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "subscriptions store: {error}"),
            Self::Clock(error) => write!(f, "subscriptions clock: {error}"),
            Self::InvalidChannelName(error) => write!(f, "invalid channel name: {error}"),
            Self::UnknownRoom(room_id) => write!(f, "not subscribed to room {room_id}"),
        }
    }
}

impl std::error::Error for SubscriptionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::InvalidChannelName(error) => Some(error),
            Self::UnknownRoom(_) => None,
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
    /// The empty display label, for a room that has no name — one
    /// addressed purely by id (dispatched into, handed over by a peer)
    /// or one whose stored label no longer parses.
    ///
    /// This is a LABEL being absent, never an identity being absent: the
    /// room is fully identified by its `RoomId`. Before ids keyed
    /// membership, an unparseable name meant the whole subscription was
    /// dropped on load — a display string could evict a real room.
    pub(crate) fn unnamed() -> Self {
        Self(String::new())
    }

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

/// Opaque mesh-identity string — WHO this scope is on the mesh.
/// Wrapper instead of a bare String so it can never be confused with
/// any other text the mesh passes around.
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

/// Resolution of a caller-supplied token against this scope's rooms.
///
/// A token is either a room id or it is not. There is no third thing:
/// an id parses to 128 bits and looks up, or the token is a name and a
/// different surface deals with it.
#[derive(Debug)]
pub enum RoomIdResolution<'a> {
    /// Does not parse as a room id.
    NotAnId,
    /// Parsed to the id of exactly this subscribed room.
    Match(&'a Subscription),
    /// A valid room id this scope is not subscribed to.
    Unknown,
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
    /// Membership in a NEWLY MINTED room. The id is generated here and
    /// is out of the caller's control.
    pub fn minting(wire_root: &Path, name: ChannelName) -> Result<Self, SubscriptionError> {
        Self::joining(wire_root, RoomId::new(), name)
    }

    /// Membership in a room whose id you already hold — the normal
    /// path. A peer hands you an id, a board row carries one, a
    /// dispatch names one; you join THAT room.
    ///
    /// `wire_root` is the account-wide machine home, not the caller's
    /// scope home: that is what makes `~/.airc`, `repo/.airc`, and every
    /// other scope on one OS account share a data plane.
    ///
    /// (This and `minting` are the only two ways to build one. There was
    /// a third, `with_wire_root`, that `joining` delegated to with an
    /// identical signature — two spellings of one constructor, so the
    /// choice between them carried no information.)
    pub fn joining(
        wire_root: &Path,
        room_id: RoomId,
        name: ChannelName,
    ) -> Result<Self, SubscriptionError> {
        // Wire dir keyed by the ID: always a valid path component, and
        // renaming a room never moves its bytes.
        let wire = wire_root.join("wires").join(room_id.to_string());
        Self::with_wire(room_id, name, wire)
    }

    pub fn with_wire(
        room_id: RoomId,
        name: ChannelName,
        wire: PathBuf,
    ) -> Result<Self, SubscriptionError> {
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
    /// Keyed by ROOM ID — the address. The name rides along inside each
    /// `Subscription` as a display label and keys nothing.
    ///
    /// This was keyed by `ChannelName`, which made a display string the
    /// lookup key for durable membership: a room could only be
    /// remembered if it had a name, a renamed room became a different
    /// room, and a room you were HANDED (dispatched into, told about by
    /// a peer) had an id and no name and so could not be represented at
    /// all. No migration is needed for the switch — `StoredSubscription`
    /// has always persisted `room_id` alongside the name, so the id was
    /// on disk the whole time; only the in-memory key was wrong.
    pub subscribed: BTreeMap<RoomId, Subscription>,
    /// Default room for `airc msg "..."` etc. `None` means no
    /// subscriptions yet (fresh-init scope).
    pub default: Option<RoomId>,
    /// Rooms the user explicitly parted. Never auto-rejoined by
    /// `join_default_context`. Re-subscribing clears the entry.
    pub parted: BTreeSet<RoomId>,
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

    /// Join the room `room_id` identifies. Idempotent: if this scope is
    /// already in that room the existing `joined_at_ms` is preserved
    /// (re-joining is a no-op for observers) and its stored label wins.
    /// Clears the room from `parted` so a later default-context
    /// re-infer keeps it.
    ///
    /// Sets the room as `default` only if no default exists yet.
    /// Explicit promotion is via [`Self::set_default`].
    pub fn join(
        &mut self,
        home: &Path,
        room_id: RoomId,
        name: ChannelName,
    ) -> Result<Subscription, SubscriptionError> {
        self.parted.remove(&room_id);
        if let Some(existing) = self.subscribed.get(&room_id) {
            return Ok(existing.clone());
        }
        let sub = Subscription::joining(home, room_id, name)?;
        self.insert(sub.clone());
        Ok(sub)
    }

    /// Mint a NEW room and join it. The id is generated by airc; the
    /// caller supplies only a display label.
    pub fn create(
        &mut self,
        home: &Path,
        name: ChannelName,
    ) -> Result<Subscription, SubscriptionError> {
        let sub = Subscription::minting(home, name)?;
        self.insert(sub.clone());
        Ok(sub)
    }

    /// Record a subscription this scope already constructed.
    pub(crate) fn insert_subscription(&mut self, sub: Subscription) {
        self.insert(sub);
    }

    fn insert(&mut self, sub: Subscription) {
        self.parted.remove(&sub.room_id);
        let room_id = sub.room_id;
        self.subscribed.insert(room_id, sub);
        if self.default.is_none() {
            self.default = Some(room_id);
        }
    }

    /// [`Self::join`] against an account-wide local wire root. See
    /// [`Subscription::with_wire_root`].
    pub fn join_with_wire_root(
        &mut self,
        wire_root: &Path,
        room_id: RoomId,
        name: ChannelName,
    ) -> Result<Subscription, SubscriptionError> {
        self.parted.remove(&room_id);
        if let Some(existing) = self.subscribed.get(&room_id) {
            return Ok(existing.clone());
        }
        let sub = Subscription::joining(wire_root, room_id, name)?;
        self.insert(sub.clone());
        Ok(sub)
    }

    /// Remove a subscription and mark it parted so it's not
    /// auto-restored. If the removed room was the default, the
    /// default falls back to any remaining subscription
    /// (deterministically the lowest-sorted id) or `None`.
    pub fn unsubscribe(&mut self, room_id: &RoomId) -> Option<Subscription> {
        let removed = self.subscribed.remove(room_id);
        if removed.is_some() {
            self.parted.insert(*room_id);
            if self.default.as_ref() == Some(room_id) {
                self.default = self.subscribed.keys().next().copied();
            }
        }
        removed
    }

    /// Set the default room. Only succeeds if the room is already
    /// subscribed; setting a non-subscribed room as default would lie
    /// about what `airc msg` will hit.
    pub fn set_default(&mut self, room_id: RoomId) -> Result<(), SubscriptionError> {
        if !self.subscribed.contains_key(&room_id) {
            return Err(SubscriptionError::UnknownRoom(room_id));
        }
        self.default = Some(room_id);
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

    /// The subscription for a room id — one comparison against the key
    /// this map is already keyed by.
    ///
    /// Borrowed, deliberately. A caller that only needs to READ a field
    /// should not be handed an owned copy of a `String` + `PathBuf`;
    /// clone at the boundary that genuinely requires ownership, and only
    /// there.
    pub fn get(&self, room_id: RoomId) -> Option<&Subscription> {
        self.subscribed.get(&room_id)
    }

    /// Resolve a join TOKEN that may be a channel ID rather than a
    /// name (card c409eaf5, octave 2 — glass-boxed 2026-08-11).
    ///
    /// `ChannelName::new` hashes whatever it is given, so an id-shaped
    /// token that reaches it mints a brand-new channel: passing
    /// `"3be59578"` (the hex prefix of academy's channel UUID) created
    /// ghost room `7d1a76de…`, and from then on this scope wrote and
    /// read a channel nobody else was in — every store read looked
    /// like a monologue and the transport took the blame. The original
    /// c409eaf5 guard refused FULL uuid strings; the 8-hex short id —
    /// the form every other id surface in the system accepts — walked
    /// straight past it into the mint.
    ///
    /// An id-shaped token is an ID in the caller's head, so treat it
    /// as one: a run of 8..=32 hex chars (dashes ignored) resolves as
    /// a prefix of a subscribed channel's UUID. Exactly one match
    /// binds to that existing room; zero or several is the caller's
    /// answer, never a mint. Anything else is a plain name.
    pub fn resolve_room_id(&self, token: &str) -> RoomIdResolution<'_> {
        // Parse to the id TYPE, then look up. The map is keyed by the
        // 16-byte id, so this is one comparison against a key — never a
        // scan, never a rendered uuid, never a substring of one.
        let Ok(uuid) = uuid::Uuid::parse_str(token.trim()) else {
            return RoomIdResolution::NotAnId;
        };
        match self.subscribed.get(&RoomId::from_uuid(uuid)) {
            Some(subscription) => RoomIdResolution::Match(subscription),
            None => RoomIdResolution::Unknown,
        }
    }

    /// The ids of every subscribed room — what a consumer surface reads
    /// to know which rooms to drain. The id is the address; a caller
    /// that wants a label reads it off the `Subscription` via
    /// [`Self::all`].
    pub fn room_ids(&self) -> impl Iterator<Item = &RoomId> {
        self.subscribed.keys()
    }
}

/// Load the subscription set from the durable store. If no rows exist
/// yet, return an empty set; callers decide how to seed it.
pub async fn load_or_init(store: &dyn EventStore) -> Result<SubscriptionSet, SubscriptionError> {
    let mut set = SubscriptionSet::empty();
    for row in store.load_subscriptions().await? {
        // Key off the ROOM ID the row already carries. The stored name is
        // read as a display label only, and a row whose label no longer
        // parses (or never had one — an id-addressed room) is still a
        // perfectly good membership: the id is what identifies it.
        if row.parted {
            set.parted.insert(row.room_id);
            continue;
        }
        let subscription = Subscription {
            name: ChannelName::new(&row.channel_name).unwrap_or_else(|_| ChannelName::unnamed()),
            room_id: row.room_id,
            wire: PathBuf::from(row.wire),
            joined_at_ms: row.joined_at_ms,
        };
        if row.is_default {
            set.default = Some(row.room_id);
        }
        set.subscribed.insert(row.room_id, subscription);
    }
    if set
        .default
        .is_some_and(|room| !set.subscribed.contains_key(&room))
    {
        set.default = set.subscribed.keys().next().copied();
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
            is_default: set.default == Some(subscription.room_id),
            parted: false,
        });
    }
    for room in &set.parted {
        if !set.subscribed.contains_key(room) {
            rows.push(StoredSubscription {
                // The parted row used to re-DERIVE its room id from the
                // name against an `unset` identity — a fabricated id that
                // matched the real room only by luck of the derivation.
                // The parted set holds the actual id now, so the row
                // carries it and the label is left empty: nothing keys on
                // a name any more.
                channel_name: String::new(),
                room_id: *room,
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

    /// True when this scope is subscribed to `room_id`.
    pub async fn is_subscribed(&self, room_id: &RoomId) -> Result<bool, AircError> {
        let set = self.subscription_set().await?;
        Ok(set.subscribed.contains_key(room_id))
    }

    /// Return the default room used by short-shape commands such as
    /// `airc msg "..."`.
    pub async fn default_room(&self) -> Result<Room, AircError> {
        self.current_room().await
    }

    /// Cursor of the newest event in a subscribed channel — via the
    /// daemon when attached (its ORM is the transcript; the local store
    /// can be empty/stale on an attached scope), the local store
    /// otherwise (card 8428ae8c).
    ///
    /// `None` means either the room has no events yet or this scope is
    /// not subscribed to it. Use [`Self::is_subscribed`] when callers
    /// need to distinguish those cases.
    pub async fn subscription_cursor(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<airc_core::TranscriptCursor>, AircError> {
        let set = self.subscription_set().await?;
        let Some(subscription) = set.subscribed.get(room_id) else {
            return Ok(None);
        };
        self.channel_latest_cursor(subscription.room_id).await
    }

    pub(crate) async fn subscribed_event_filter(
        &self,
        mut filter: EventFilter,
    ) -> Result<EventFilter, AircError> {
        if filter.channel.is_some() || !filter.channels.is_empty() {
            return Ok(filter);
        }
        filter.channels = self.subscribed_room_ids().await?.into_iter().collect();
        Ok(filter)
    }

    async fn subscribed_room_ids(&self) -> Result<Vec<RoomId>, AircError> {
        let mut room_ids = Vec::new();
        let mut seen = HashSet::new();
        let set = self.subscription_set().await?;
        for subscription in set.all() {
            if seen.insert(subscription.room_id) {
                room_ids.push(subscription.room_id);
            }
        }
        // No conjured default. A scope with no subscriptions is in no
        // rooms, and an empty filter says exactly that — inventing a
        // room id here would have this scope draining a room nobody
        // put it in.
        Ok(room_ids)
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
    use airc_core::{Body, ClientId, EventId, Headers, MentionTarget, PeerId, TranscriptEvent};
    use airc_store::InMemoryEventStore;
    use tempfile::tempdir;

    fn make_event(lamport: u64, room_id: RoomId, body: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id,
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind: airc_core::TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text(body)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

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
    fn resolve_id_token_binds_id_shaped_tokens_to_existing_channels() {
        // what this catches: the ghost-room mint (card c409eaf5 octave 2,
        // 2026-08-11) — an 8-hex short id of a SUBSCRIBED channel passed as a
        // join token must resolve to that channel, and an id-shaped token
        // matching nothing must be Unknown (refused upstream), never a fresh
        // v5 mint that splits the room's readers from its writers.
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mut set = SubscriptionSet::empty();
        let academy = set
            .create(home, ChannelName::new("academy").unwrap())
            .unwrap();

        // A room id → binds to academy.
        match set.resolve_room_id(&academy.room_id.to_string()) {
            RoomIdResolution::Match(sub) => assert_eq!(sub.name.as_str(), "academy"),
            other => panic!("full uuid must Match, got {other:?}"),
        }
        // A REAL id naming no subscribed room → Unknown (join refuses; no mint).
        assert!(matches!(
            set.resolve_room_id(&RoomId::new().to_string()),
            RoomIdResolution::Unknown
        ));
        // A TRUNCATION of a real id is not an id. It was accepted once, by
        // rendering every room's uuid to text and prefix-matching it — which
        // made 32 hex characters mean 128 bits only when they happened not to
        // collide, and needed an `Ambiguous` arm to paper over when they did.
        // A key is passed whole or it is not passed.
        let simple = academy.room_id.as_uuid().as_simple().to_string();
        assert!(matches!(
            set.resolve_room_id(&simple[..8]),
            RoomIdResolution::NotAnId
        ));
        assert!(matches!(
            set.resolve_room_id("deadbeef"),
            RoomIdResolution::NotAnId
        ));
        // Plain names — including hexy-but-short and dashed ones — stay names.
        assert!(matches!(
            set.resolve_room_id("academy"),
            RoomIdResolution::NotAnId
        ));
        assert!(matches!(
            set.resolve_room_id("bench-swe-run-1"),
            RoomIdResolution::NotAnId
        ));
        assert!(matches!(
            set.resolve_room_id("cafe"),
            RoomIdResolution::NotAnId
        ));
    }

    #[test]
    fn create_mints_a_distinct_room_each_time_and_seeds_default() {
        // what this catches: create keyed by NAME. Two rooms with the same
        // label are two rooms; collapsing them was the v5(name) collision.
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mut set = SubscriptionSet::empty();
        assert!(set.default.is_none());

        let first = set
            .create(home, ChannelName::new("general").unwrap())
            .unwrap();
        assert_eq!(set.default, Some(first.room_id), "first room seeds default");

        let second = set
            .create(home, ChannelName::new("general").unwrap())
            .unwrap();
        assert_ne!(first.room_id, second.room_id);
        assert_eq!(set.subscribed.len(), 2);
        assert_eq!(set.default, Some(first.room_id), "default is not stolen");
    }

    #[test]
    fn join_is_idempotent_on_the_id_and_keeps_the_stored_label() {
        // what this catches: re-joining a room you are already in must be a
        // no-op for observers (joined_at_ms preserved), and a caller's label
        // must never overwrite the one already stored.
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mut set = SubscriptionSet::empty();
        let room = set
            .create(home, ChannelName::new("general").unwrap())
            .unwrap();

        let again = set
            .join(home, room.room_id, ChannelName::new("renamed").unwrap())
            .unwrap();
        assert_eq!(again, room);
        assert_eq!(set.subscribed.len(), 1);
    }

    #[test]
    fn join_with_wire_root_uses_the_machine_account_wire_keyed_by_id() {
        // what this catches: a per-scope wire root (which split one machine's
        // data plane per project dir) and a name-keyed wire dir.
        let scope_a = tempdir().unwrap();
        let scope_b = tempdir().unwrap();
        let machine_home = tempdir().unwrap();
        let room_id = RoomId::new();
        let mut a = SubscriptionSet::empty();
        let mut b = SubscriptionSet::empty();

        let a_sub = a
            .join_with_wire_root(
                machine_home.path(),
                room_id,
                ChannelName::new("general").unwrap(),
            )
            .unwrap();
        let b_sub = b
            .join_with_wire_root(
                machine_home.path(),
                room_id,
                ChannelName::new("general").unwrap(),
            )
            .unwrap();

        assert_eq!(a_sub.room_id, b_sub.room_id);
        assert_eq!(a_sub.wire, b_sub.wire);
        assert_eq!(
            a_sub.wire,
            machine_home.path().join("wires").join(room_id.to_string())
        );
        assert!(
            !a_sub.wire.starts_with(scope_a.path()) && !b_sub.wire.starts_with(scope_b.path()),
            "same-machine account mesh must not isolate local data-plane per project scope"
        );
    }

    #[test]
    fn a_room_with_no_label_is_still_a_membership() {
        // what this catches: dropping a room because its label is absent or
        // unparseable. The id identifies it; the label is decoration.
        let dir = tempdir().unwrap();
        let mut set = SubscriptionSet::empty();
        let room = set.create(dir.path(), ChannelName::unnamed()).unwrap();
        assert!(set.subscribed.contains_key(&room.room_id));
        assert_eq!(set.default, Some(room.room_id));
    }

    #[test]
    fn unsubscribe_marks_parted_by_id_and_falls_back_default() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mut set = SubscriptionSet::empty();
        let general = set
            .create(home, ChannelName::new("general").unwrap())
            .unwrap();
        let cambriantech = set
            .create(home, ChannelName::new("cambriantech").unwrap())
            .unwrap();

        let removed = set
            .unsubscribe(&general.room_id)
            .expect("general was subscribed");
        assert_eq!(removed.room_id, general.room_id);
        assert!(set.parted.contains(&general.room_id));
        assert_eq!(set.default, Some(cambriantech.room_id));
        assert_eq!(set.subscribed.len(), 1);
    }

    #[test]
    fn rejoining_a_parted_room_clears_the_tombstone() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let mut set = SubscriptionSet::empty();
        let general = set
            .create(home, ChannelName::new("general").unwrap())
            .unwrap();
        set.unsubscribe(&general.room_id);
        assert!(set.parted.contains(&general.room_id));

        set.join(home, general.room_id, general.name.clone())
            .unwrap();
        assert!(!set.parted.contains(&general.room_id));
        assert_eq!(set.default, Some(general.room_id));
    }

    #[test]
    fn set_default_requires_membership_and_names_the_id_it_refused() {
        let dir = tempdir().unwrap();
        let mut set = SubscriptionSet::empty();
        set.create(dir.path(), ChannelName::new("general").unwrap())
            .unwrap();

        let stranger = RoomId::new();
        match set.set_default(stranger) {
            Err(SubscriptionError::UnknownRoom(id)) => assert_eq!(id, stranger),
            other => panic!("must refuse a non-subscribed room by id, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn save_load_round_trip_preserves_set() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let store = InMemoryEventStore::new();
        let mut set = SubscriptionSet::empty();
        set.create(home, ChannelName::new("general").unwrap())
            .unwrap();
        set.create(home, ChannelName::new("cambriantech").unwrap())
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
        // Set, not sequence. The old assertion read alphabetically only
        // because the map was keyed by the NAME; keyed by id, iteration
        // order follows v4 ids and carries no meaning. A caller that
        // wants rooms in a particular order sorts by the field it means.
        let mut names = subscriptions
            .iter()
            .map(|subscription| subscription.name.as_str())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["cambriantech", "general"]);

        // Membership is asked BY ID — the labels above are only how the
        // rooms were created; they answer no question about membership.
        let by_label = |label: &str| {
            subscriptions
                .iter()
                .find(|s| s.name.as_str() == label)
                .expect("created above")
                .room_id
        };
        let cambriantech = by_label("cambriantech");
        let general = by_label("general");
        let missing = RoomId::new();

        assert!(airc.is_subscribed(&cambriantech).await.unwrap());
        assert!(airc.is_subscribed(&general).await.unwrap());
        assert!(!airc.is_subscribed(&missing).await.unwrap());

        let default = airc.default_room().await.unwrap();
        assert_eq!(default.name, "cambriantech");

        let next_lamport = airc
            .subscription_cursor(&cambriantech)
            .await
            .unwrap()
            .map_or(42, |cursor| cursor.lamport + 1);
        let cambriantech_event = make_event(next_lamport, default.channel, "cursor proof");
        let expected_cursor = cambriantech_event.cursor();
        airc.append_event(cambriantech_event).await.unwrap();
        assert_eq!(
            airc.subscription_cursor(&cambriantech).await.unwrap(),
            Some(expected_cursor)
        );
        assert!(airc.subscription_cursor(&missing).await.unwrap().is_none());
    }
}
