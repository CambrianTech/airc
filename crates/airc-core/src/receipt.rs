//! Receipts — delivered / read / applied acknowledgments tied to a
//! specific transcript event.
//!
//! Carried in a separate transcript event of kind `Receipt` so the receipt
//! itself is durable + cursor-paged like any other event. Consumers that
//! need read-state per peer aggregate these.

use serde::{Deserialize, Serialize};

use crate::ids::{ClientId, EventId, PeerId};

/// What kind of acknowledgment this receipt represents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    /// Receiver's transport accepted the envelope (wire-level confirmation).
    Delivered,
    /// Receiver's UI / consumer surfaced the message to a human.
    Read,
    /// Consumer-level "applied" semantics (used e.g. for commands the
    /// recipient agent has executed).
    Applied,
}

/// One receipt tied to one earlier transcript event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Which event this receipt acknowledges.
    pub event_id: EventId,
    /// Which peer is acknowledging.
    pub peer_id: PeerId,
    /// Which client/session under that peer is acknowledging (multi-tab
    /// disambiguation — one peer may read on phone but not desktop yet).
    pub client_id: ClientId,
    /// Kind of acknowledgment.
    pub kind: ReceiptKind,
    /// When the acknowledgment was generated (receiver-local wall clock,
    /// milliseconds since epoch).
    pub received_at_ms: u64,
}
