//! Room value type — a room's concrete substrate location.
//!
//! A room IS its `RoomId`. The id is minted by airc when the room is
//! created, is read-only to every caller, and is the only thing that
//! addresses the room:
//!   - wire = `<home>/wires/<room-id>/`
//!   - name = a display label hanging off the id, addressing nothing
//!
//! Peers exchange the id. Named lookup for humans lives at the CLI
//! edge, where a label is resolved against rooms this scope is already
//! in — and refuses when the label is ambiguous, because a label is not
//! an address.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::RoomId;

const ROOM_VERSION: u32 = 1;

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

/// A room's concrete substrate location.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Room {
    /// Schema version.
    pub version: u32,
    /// Display label. Carries no addressing role; two rooms may share
    /// one, and a room may have none.
    pub name: String,
    /// Wire directory for this room, keyed by its id.
    pub wire: PathBuf,
    /// The room's ADDRESS. Minted by airc, read-only to callers.
    pub channel: RoomId,
    pub joined_at_ms: u64,
}

impl Room {
    /// Mint a NEW room. The id is generated here and is out of the
    /// caller's control; the name is a display label.
    ///
    /// Nothing derives the id, so nothing can diverge from it, no
    /// id-SHAPED text can collide with it, and the label is free text
    /// because no one hashes it. Renaming is a label edit.
    pub fn mint(home: &Path, name: &str) -> Result<Self, RoomError> {
        let channel = RoomId::from_uuid(Uuid::new_v4());
        // Wire dir keyed by the ID: always a valid path component, so
        // no name-sanitiser has a say in where a room's bytes live.
        let wire = home.join("wires").join(channel.to_string());
        Ok(Self {
            version: ROOM_VERSION,
            name: name.to_string(),
            wire,
            channel,
            joined_at_ms: now_ms()?,
        })
    }

    /// Stamp this room's display NAME onto outbound headers
    /// ([`airc_protocol::HEADER_AIRC_CHANNEL_NAME`]) so a receiving
    /// surface can render a label next to the id. Never overwrites a
    /// caller-supplied value; skips unnamed rooms. Delivery routes on
    /// the id — this header is for humans reading the wire.
    pub fn stamp_name_header(&self, headers: &mut airc_core::Headers) {
        if self.name.is_empty() {
            return;
        }
        headers
            .entry(airc_protocol::HEADER_AIRC_CHANNEL_NAME.to_string())
            .or_insert_with(|| self.name.clone());
    }
}

fn now_ms() -> Result<u64, std::time::SystemTimeError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64)
}

#[cfg(test)]
mod tests;
