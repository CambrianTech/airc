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
    /// Channel UUID, derived deterministically from `name` so peers
    /// joining the same name land in the same channel.
    pub channel: RoomId,
    pub joined_at_ms: u64,
}

impl Room {
    /// Derive a `Room` from a name + home dir. Deterministic — same
    /// (home, name) always produces the same `Room`. Doesn't read
    /// or write disk.
    pub fn from_name(home: &Path, name: &str) -> Result<Self, RoomError> {
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

    /// Default room — auto-created on `airc init`. Name "default",
    /// derived per `from_name`.
    pub fn default_for(home: &Path) -> Result<Self, RoomError> {
        Self::from_name(home, DEFAULT_ROOM_NAME)
    }

    /// A `Room` handle for a channel we did NOT derive ourselves — the
    /// channel is adopted verbatim instead of re-derived from `name`.
    ///
    /// This exists for replying into the channel a REQUEST arrived on.
    /// Identity-scoped channel derivation can diverge between machines
    /// (the M5↔bigmama blind-room bug), and two scopes on one machine
    /// can be parked in different rooms — a responder that answers into
    /// its own current room talks past a requester listening where it
    /// asked. The request event's `room_id` is the one channel both
    /// sides provably share; use it as-is.
    pub fn at_channel(home: &Path, name: &str, channel: RoomId) -> Result<Self, RoomError> {
        let mut room = Self::from_name(home, name)?;
        room.channel = channel;
        Ok(room)
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
