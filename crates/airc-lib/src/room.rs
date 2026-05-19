//! Current-room persistence — the default `(wire, channel)` for
//! short-shape commands like `airc-rs msg "hi"` and `airc-rs inbox`.
//!
//! AI peers (and humans) shouldn't have to type `--wire <path>
//! --channel <uuid>` on every call. Joining a room writes
//! `<home>/room.json` once; every subsequent command reads it.
//! `airc-rs join <name>` switches.
//!
//! A "room" is a name. The substrate primitives it expands to are
//! deterministic:
//!   - wire    = `<home>/wires/<name>/`
//!   - channel = UUIDv5(namespace=oid, name)
//!
//! Same name → same channel UUID across machines, so two peers who
//! both `airc-rs join project-x` land in the same room without
//! exchanging the channel UUID.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::RoomId;

const ROOM_FILENAME: &str = "room.json";
const ROOM_VERSION: u32 = 1;
const DEFAULT_ROOM_NAME: &str = "default";

/// Namespace UUID for deriving channel UUIDs from room names.
/// Stable across all airc-rs installs so `airc-rs join project-x`
/// on different machines produces the same channel.
const ROOM_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xc2, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// What can go wrong loading / saving the current room.
#[derive(Debug)]
pub enum RoomError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// Schema version mismatch — refuse to misinterpret old / future
    /// room files.
    SchemaVersionMismatch {
        found: u32,
        expected: u32,
    },
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoomError::Io(error) => write!(f, "room I/O: {error}"),
            RoomError::Json(error) => write!(f, "room JSON: {error}"),
            RoomError::SchemaVersionMismatch { found, expected } => {
                write!(f, "room.json version {found}, expected {expected}")
            }
        }
    }
}

impl std::error::Error for RoomError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RoomError::Io(error) => Some(error),
            RoomError::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RoomError {
    fn from(error: std::io::Error) -> Self {
        RoomError::Io(error)
    }
}

impl From<serde_json::Error> for RoomError {
    fn from(error: serde_json::Error) -> Self {
        RoomError::Json(error)
    }
}

/// The current room — what `airc-rs msg` / `inbox` / `send` /
/// `listen` default to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Room {
    /// Schema version.
    pub version: u32,
    /// Human-readable room name. `default` on fresh init.
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
    pub fn from_name(home: &Path, name: &str) -> Self {
        let wire = home.join("wires").join(sanitise_name(name));
        let channel = RoomId::from_uuid(Uuid::new_v5(&ROOM_NAMESPACE, name.as_bytes()));
        Self {
            version: ROOM_VERSION,
            name: name.to_string(),
            wire,
            channel,
            joined_at_ms: now_ms(),
        }
    }

    /// Default room — auto-created on `airc-rs init`. Name "default",
    /// derived per `from_name`.
    pub fn default_for(home: &Path) -> Self {
        Self::from_name(home, DEFAULT_ROOM_NAME)
    }
}

/// Path to `<home>/room.json`.
pub fn path_in(home: &Path) -> PathBuf {
    home.join(ROOM_FILENAME)
}

/// Load the current room. Falls back to the default room (`name =
/// "default"`) if `<home>/room.json` doesn't exist yet — this is
/// the normal state for a fresh install.
pub fn load_or_default(home: &Path) -> Result<Room, RoomError> {
    let path = path_in(home);
    if !path.exists() {
        return Ok(Room::default_for(home));
    }
    let text = std::fs::read_to_string(&path)?;
    let room: Room = serde_json::from_str(&text)?;
    if room.version != ROOM_VERSION {
        return Err(RoomError::SchemaVersionMismatch {
            found: room.version,
            expected: ROOM_VERSION,
        });
    }
    Ok(room)
}

/// Save the current room (overwriting any previous one).
pub fn save(home: &Path, room: &Room) -> Result<(), RoomError> {
    std::fs::create_dir_all(home)?;
    let path = path_in(home);
    let text = serde_json::to_string_pretty(room)?;
    std::fs::write(&path, text)?;
    set_owner_only_permissions(&path)?;
    Ok(())
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH violates AIRC room timestamp contract")
        .as_millis() as u64
}

#[cfg(test)]
mod tests;
