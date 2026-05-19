//! Self-echo filtering — keeps a consumer from seeing its own broadcasts.
//!
//! When a peer sends a message, the wire envelope flows out to all room
//! participants AND mirrors back to the sender's own log. Without
//! filtering, a chat client renders its own outgoing message twice. The
//! distinction between `ExcludeSameClient` and `ExcludeSamePeer` matters
//! for multi-tab sessions: a peer on phone may legitimately want to see
//! what their laptop just sent (`ExcludeSameClient` keeps it visible),
//! whereas a server-side aggregation may want to exclude anything the
//! same identity sent (`ExcludeSamePeer`).

use serde::{Deserialize, Serialize};

use crate::ids::{ClientId, PeerId};
use crate::transcript::TranscriptEvent;

/// How aggressively to filter the receiver's own events out of display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfFilter {
    /// Show everything, including the receiver's own broadcasts.
    IncludeAll,
    /// Hide events originating from the SAME client/tab — preserves
    /// cross-tab visibility (phone sees what laptop sent).
    ExcludeSameClient,
    /// Hide ALL events from the receiver's peer identity, regardless of
    /// which tab/client emitted them.
    ExcludeSamePeer,
}

/// Filter an event stream by the receiver's identity + filter mode.
pub fn filter_self_echoes(
    events: impl IntoIterator<Item = TranscriptEvent>,
    peer_id: &PeerId,
    client_id: &ClientId,
    filter: SelfFilter,
) -> Vec<TranscriptEvent> {
    events
        .into_iter()
        .filter(|event| !event.is_self_echo(peer_id, client_id, filter))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::Body;
    use crate::ids::{EventId, RoomId};
    use crate::transcript::{MentionTarget, TranscriptKind};

    /// Test helper: deterministic event with caller-supplied peer/client
    /// UUIDs so assertions can name them.
    fn event_at(seed: u128, lamport: u64, peer_id: PeerId, client_id: ClientId) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::from_u128(seed),
            room_id: RoomId::from_u128(0xc0ffee),
            peer_id,
            client_id,
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            headers: Default::default(),
            body: Some(Body::text(format!("message {seed:x}"))),
            attachment: None,
            receipt: None,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn self_filter_distinguishes_peer_from_client() {
        let agent = PeerId::from_u128(0xa6e7);
        let reviewer = PeerId::from_u128(0x5e7e);
        let tab_a = ClientId::from_u128(0xa);
        let tab_b = ClientId::from_u128(0xb);
        let tab_c = ClientId::from_u128(0xc);

        let same_client = event_at(0x01, 1, agent, tab_a);
        let same_peer_other_client = event_at(0x02, 2, agent, tab_b);
        let other_peer = event_at(0x03, 3, reviewer, tab_c);
        let events = vec![
            same_client.clone(),
            same_peer_other_client.clone(),
            other_peer.clone(),
        ];

        // ExcludeSameClient: drop only events from `tab_a`. Same-peer
        // other-client survives; other-peer survives.
        let client_filtered = filter_self_echoes(
            events.clone(),
            &agent,
            &tab_a,
            SelfFilter::ExcludeSameClient,
        );
        let ids: Vec<_> = client_filtered.iter().map(|e| e.event_id).collect();
        assert_eq!(
            ids,
            vec![same_peer_other_client.event_id, other_peer.event_id]
        );

        // ExcludeSamePeer: drop every event from `agent`, regardless of tab.
        let peer_filtered = filter_self_echoes(events, &agent, &tab_a, SelfFilter::ExcludeSamePeer);
        let ids: Vec<_> = peer_filtered.iter().map(|e| e.event_id).collect();
        assert_eq!(ids, vec![other_peer.event_id]);
    }
}
