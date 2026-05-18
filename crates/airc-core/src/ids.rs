//! Newtype-wrapped identifier strings.
//!
//! Each id is a thin wrapper around a `String`. The `#[serde(transparent)]`
//! attribute means the JSON shape is just the raw string — no `{"value": ...}`
//! envelope — so wire compatibility with the Python+bash airc is preserved.
//!
//! These types intentionally do NOT enforce a specific format (uuid, base64,
//! human-hash) at the type level. Consumers may pick the encoding they want;
//! the substrate just routes opaque strings keyed on these ids.

use serde::{Deserialize, Serialize};

/// Stable identifier for one transcript event. Persists across host
/// migrations and replay — two peers receiving the same wire envelope
/// see the same `EventId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub String);

/// A room / channel handle. Consumers may pick any naming scheme; airc
/// does not interpret the inner string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomId(pub String);

/// A peer identifier — the canonical "who is this." Multiple `ClientId`s
/// may share one `PeerId` (multi-tab same-identity case).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerId(pub String);

/// A per-process / per-tab session identifier under a peer. The pair
/// `(PeerId, ClientId)` uniquely identifies one running airc consumer
/// session. Used for self-echo filtering when multiple sessions share
/// a nick.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientId(pub String);

/// Stable handle for an attached file or media blob.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileId(pub String);

/// Content-addressed hash of a blob. Format-neutral string (typically
/// `"sha256:<hex>"` but consumers may use other prefixes for other
/// algorithms — airc just matches strings).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(pub String);
