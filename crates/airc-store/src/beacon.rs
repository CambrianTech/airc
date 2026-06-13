//! Store-owned account-mesh presence beacons.
//!
//! Beacons are runtime presence state. The library layer may render
//! them as coordinator snapshots, but the durable source of truth is
//! the store, not per-peer JSON files.

use airc_core::PeerId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBeacon {
    pub mesh_identity: String,
    pub peer_id: PeerId,
    pub scope_home: String,
    pub subscribed_channels: Vec<String>,
    pub pid: u32,
    pub published_at_ms: u64,
    pub heartbeat_at_ms: u64,
}
