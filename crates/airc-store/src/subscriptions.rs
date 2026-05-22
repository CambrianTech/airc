//! Subscription store types.

use airc_core::RoomId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSubscription {
    pub channel_name: String,
    pub room_id: RoomId,
    pub wire: String,
    pub joined_at_ms: u64,
    pub is_default: bool,
    pub parted: bool,
}
