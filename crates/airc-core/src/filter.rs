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

use crate::headers::HEADER_AIRC_CLIENT;
use crate::ids::{ClientId, PeerId};
use crate::transcript::TranscriptEvent;

/// One runtime's own sends, on a durable identity it may share with other
/// runtimes (several agent sessions under one HOME share peer and client ids).
///
/// THE self-echo rule, in one place: a stamped `airc.client` header is
/// authoritative (self iff it names this runtime; a different tag, or no local
/// tag, stays visible regardless of either durable id); an unstamped event is
/// self iff its durable `client_id` is this one. Live delivery, the addressed
/// history page and the store-side history count all apply this value; the
/// stores translate it to their columns and header predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSelf {
    pub client_id: ClientId,
    pub runtime_client: Option<String>,
}

impl RuntimeSelf {
    pub fn new(client_id: ClientId, runtime_client: Option<&str>) -> Self {
        Self {
            client_id,
            runtime_client: runtime_client.map(str::to_string),
        }
    }

    pub fn is_self_event(&self, event: &TranscriptEvent) -> bool {
        self.is_self(&event.headers, event.client_id)
    }

    /// The rule over the two facts it reads, for stores that hold envelopes.
    pub fn is_self(&self, headers: &crate::Headers, client_id: ClientId) -> bool {
        // Historical unstamped events retain the narrower client fallback. It is
        // still ambiguous on a shared HOME; peer equality cannot resolve that
        // ambiguity and must never hide another runtime's header.
        match headers.get(HEADER_AIRC_CLIENT) {
            Some(tag) => self.runtime_client.as_deref() == Some(tag.as_str()),
            None => client_id == self.client_id,
        }
    }
}

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
    use crate::headers::Headers;
    use crate::ids::{EventId, RoomId};
    use crate::transcript::{MentionTarget, TranscriptKind};

    /// Test helper: deterministic event with caller-supplied peer/client
    /// UUIDs so assertions can name them.
    fn event_at(seed: u128, lamport: u64, peer_id: PeerId, client_id: ClientId) -> TranscriptEvent {
        TranscriptEvent {
            class: crate::EventClass::Correspondence(crate::CorrespondenceKind::Message),
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

    #[test]
    fn runtime_self_header_is_authoritative_across_shared_identities() {
        let local_client = ClientId::new();
        let local_peer = PeerId::new();
        let self_filter = RuntimeSelf::new(local_client, Some("codex:thread-1"));
        let no_runtime = RuntimeSelf::new(local_client, None);
        let mut event = TranscriptEvent {
            class: crate::EventClass::Correspondence(crate::CorrespondenceKind::Message),
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: local_peer,
            client_id: local_client,
            kind: TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: 1,
            target: MentionTarget::All,
            headers: Headers::new(),
            // Identity-like body content is deliberately irrelevant. The
            // predicate must not sniff or deserialize it to determine self.
            body: Some(Body::text(r#"{"airc.client":"codex:thread-1"}"#)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };

        for peer_id in [local_peer, PeerId::new()] {
            for client_id in [local_client, ClientId::new()] {
                event.peer_id = peer_id;
                event.client_id = client_id;
                event
                    .headers
                    .insert(HEADER_AIRC_CLIENT.to_string(), "codex:thread-1".to_string());
                assert!(self_filter.is_self_event(&event));
                assert!(!no_runtime.is_self_event(&event));

                event.headers.insert(
                    HEADER_AIRC_CLIENT.to_string(),
                    "claude:session-1".to_string(),
                );
                assert!(!self_filter.is_self_event(&event));
                assert!(!no_runtime.is_self_event(&event));

                // An unrelated header cannot impersonate the runtime tag;
                // unstamped events keep the existing durable-client fallback.
                event.headers.remove(HEADER_AIRC_CLIENT);
                event
                    .headers
                    .insert("other.client".to_string(), "codex:thread-1".to_string());
                assert_eq!(self_filter.is_self_event(&event), client_id == local_client);
                assert_eq!(no_runtime.is_self_event(&event), client_id == local_client);
            }
        }
    }
}
