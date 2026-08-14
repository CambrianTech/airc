//! Room value type — the substrate expansion of a channel name into
//! a wire path and channel id.
//!
//! A "room" is a name. The substrate primitives it expands to are
//! deterministic:
//!   - wire    = `<home>/wires/<name>/`
//!   - channel = UUIDv5(namespace=oid, name)
//!
//! Same name → same channel UUID across machines, so two peers who
//! both `airc join project-x` land in the same room without
//! exchanging the channel UUID.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::RoomId;

const ROOM_VERSION: u32 = 1;
const DEFAULT_ROOM_NAME: &str = "default";

/// Namespace UUID for deriving channel UUIDs from room names.
/// Stable across all airc installs so `airc join project-x`
/// on different machines produces the same channel.
const ROOM_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xc2, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// What can go wrong constructing a room value.
#[derive(Debug)]
pub enum RoomError {
    Clock(std::time::SystemTimeError),
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoomError::Clock(error) => write!(f, "room timestamp clock error: {error}"),
        }
    }
}

impl std::error::Error for RoomError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RoomError::Clock(error) => Some(error),
        }
    }
}

impl From<std::time::SystemTimeError> for RoomError {
    fn from(error: std::time::SystemTimeError) -> Self {
        RoomError::Clock(error)
    }
}

/// A channel's concrete substrate location.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Room {
    /// Schema version.
    pub version: u32,
    /// Human-readable room name.
    pub name: String,
    /// Wire directory for this room.
    pub wire: PathBuf,
    /// Channel UUID — the room's ADDRESS. Minted, never derived from
    /// the name (see `mint`). The name is a label hanging off this id;
    /// this id is what every caller passes.
    pub channel: RoomId,
    pub joined_at_ms: u64,
}

impl Room {
    /// Mint a NEW room. The id is generated and out of the caller's
    /// control; the name is a display label with no addressing role.
    ///
    /// ## Why the id is minted and not derived
    ///
    /// This used to be `from_name`, deriving BOTH the identity
    /// (`new_v5(ROOM_NAMESPACE, name)`) and the storage location
    /// (`wires/sanitise_name(name)`) from a human string. One
    /// derivation, and text became identity — which is the root of
    /// every room defect this crate carries:
    ///
    /// - a name that is *id-shaped* hashes into a brand-new channel,
    ///   so a scope writes and reads a room nobody else is in. Twice
    ///   in production (card c409eaf5): a full uuid, then the 8-hex
    ///   short id minting ghost room `7d1a76de` off academy's prefix.
    ///   `resolve_id_token` and the `JoinUuidString` / `JoinIdUnknown`
    ///   / `JoinIdAmbiguous` errors exist ONLY to referee that.
    /// - an id can *diverge* from its stored name, so join carries
    ///   self-healing (`rebind_diverged`) to re-bind rooms whose uuid
    ///   no longer derives from the name it was saved under.
    /// - the name charset (`ChannelName`) becomes load-bearing on
    ///   identity, so a room cannot be called anything a hash-input
    ///   validator dislikes.
    ///
    /// A minted id makes all of that unrepresentable: nothing derives,
    /// so nothing can diverge; the id IS an id, so nothing can be
    /// merely id-SHAPED; the label is free text because no one hashes
    /// it. Renaming a room is now a label edit, and two rooms may
    /// share a label without colliding.
    ///
    /// Rendezvous — the one thing derivation genuinely bought (two
    /// machines independently landing on `#general` with no registry)
    /// — is a discovery concern, not an identity one: peers exchange
    /// the id. Named lookup for humans lives at the CLI edge.
    pub fn mint(home: &Path, name: &str) -> Result<Self, RoomError> {
        let channel = RoomId::from_uuid(Uuid::new_v4());
        // Wire dir keyed by the ID: always a valid path component, so
        // `sanitise_name` has no say in where a room's bytes live.
        let wire = home.join("wires").join(channel.to_string());
        Ok(Self {
            version: ROOM_VERSION,
            name: name.to_string(),
            wire,
            channel,
            joined_at_ms: now_ms()?,
        })
    }

    /// Re-derive the legacy name-hashed room. RETAINED FOR MIGRATION
    /// ONLY: every room created before ids were minted has an id that
    /// IS `v5(name)` and a wire dir at `wires/<sanitised-name>`, so
    /// existing installs (and peers still on the old build) must be
    /// able to reconstruct it to find their bytes. Never call this for
    /// a NEW room — `mint` is the constructor.
    pub fn legacy_from_name(home: &Path, name: &str) -> Result<Self, RoomError> {
        let wire = home.join("wires").join(sanitise_name(name));
        let channel = RoomId::from_uuid(Uuid::new_v5(&ROOM_NAMESPACE, name.as_bytes()));
        Ok(Self {
            version: ROOM_VERSION,
            name: name.to_string(),
            wire,
            channel,
            joined_at_ms: now_ms()?,
        })
    }

    /// Default room — auto-created on `airc init`.
    ///
    /// LEGACY-DERIVED on purpose: every install that already ran `init`
    /// has this room at `v5("default")` with its bytes under
    /// `wires/default`. Minting a fresh id here would strand those
    /// bytes and split existing scopes from their own default room, so
    /// this one keeps the derived id until a migration moves it.
    pub fn default_for(home: &Path) -> Result<Self, RoomError> {
        Self::legacy_from_name(home, DEFAULT_ROOM_NAME)
    }

    /// Stamp this room's human NAME onto outbound headers
    /// ([`airc_protocol::HEADER_AIRC_CHANNEL_NAME`]) — the convergence
    /// key that lets a receiving machine whose identity-scoped channel
    /// derivation diverged from ours (the M5↔bigmama blind-room bug)
    /// re-derive the room under its own identity and still deliver.
    /// Never overwrites a caller-supplied value; skips unnamed rooms.
    pub fn stamp_name_header(&self, headers: &mut airc_core::Headers) {
        if self.name.is_empty() {
            return;
        }
        headers
            .entry(airc_protocol::HEADER_AIRC_CHANNEL_NAME.to_string())
            .or_insert_with(|| self.name.clone());
    }
}

/// Sanitise a room name into a path-safe directory component. ASCII
/// alphanumerics + `-` + `_` survive; everything else becomes `-`.
/// Multiple names can collide post-sanitisation (`foo/bar` and
/// `foo-bar` → same dir); avoid weird names.
fn sanitise_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn now_ms() -> Result<u64, std::time::SystemTimeError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64)
}

#[cfg(test)]
mod tests;
