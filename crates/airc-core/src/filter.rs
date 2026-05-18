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

    fn event(id: &str, lamport: u64, peer: &str, client: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId(id.to_string()),
            room_id: RoomId("general".to_string()),
            peer_id: PeerId(peer.to_string()),
            client_id: ClientId(client.to_string()),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            body: Some(Body::text(format!("message {id}"))),
            attachment: None,
            receipt: None,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn self_filter_distinguishes_peer_from_client() {
        let peer = PeerId("agent".to_string());
        let current_client = ClientId("tab-a".to_string());
        let events = vec![
            event("same-client", 1, "agent", "tab-a"),
            event("same-peer-other-client", 2, "agent", "tab-b"),
            event("other-peer", 3, "reviewer", "tab-c"),
        ];

        let client_filtered = filter_self_echoes(
            events.clone(),
            &peer,
            &current_client,
            SelfFilter::ExcludeSameClient,
        );
        assert_eq!(
            client_filtered
                .iter()
                .map(|e| &e.event_id.0)
                .collect::<Vec<_>>(),
            vec!["same-peer-other-client", "other-peer"]
        );

        let peer_filtered = filter_self_echoes(
            events,
            &peer,
            &current_client,
            SelfFilter::ExcludeSamePeer,
        );
        assert_eq!(
            peer_filtered
                .iter()
                .map(|e| &e.event_id.0)
                .collect::<Vec<_>>(),
            vec!["other-peer"]
        );
    }
}
