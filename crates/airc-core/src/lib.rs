//! Typed AIRC core substrate primitives.
//!
//! This crate is intentionally storage-neutral. SQLite, GitHub gists, local
//! files, and future transports adapt to these types instead of owning the
//! transcript model. Continuum should consume generated/API shapes built from
//! these Rust contracts, not query AIRC storage directly.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptKind {
    Message,
    Attachment,
    Receipt,
    Presence,
    SessionControl,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MentionTarget {
    All,
    Peer(PeerId),
    Room(RoomId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    Delivered,
    Read,
    Applied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentManifest {
    pub file_id: FileId,
    pub name: String,
    pub media_type: Option<String>,
    pub size_bytes: u64,
    pub content_hash: ContentHash,
    pub local_path: Option<String>,
    pub remote_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub event_id: EventId,
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub kind: ReceiptKind,
    pub received_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptEvent {
    pub event_id: EventId,
    pub room_id: RoomId,
    pub peer_id: PeerId,
    pub client_id: ClientId,
    pub kind: TranscriptKind,
    pub occurred_at_ms: u64,
    pub lamport: u64,
    pub target: MentionTarget,
    pub body: Option<String>,
    pub attachment: Option<AttachmentManifest>,
    pub receipt: Option<Receipt>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptCursor {
    pub lamport: u64,
    pub event_id: EventId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub room_id: RoomId,
    pub events: Vec<TranscriptEvent>,
    pub newer: Option<TranscriptCursor>,
    pub older: Option<TranscriptCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfFilter {
    IncludeAll,
    ExcludeSameClient,
    ExcludeSamePeer,
}

impl TranscriptEvent {
    pub fn cursor(&self) -> TranscriptCursor {
        TranscriptCursor {
            lamport: self.lamport,
            event_id: self.event_id.clone(),
        }
    }

    pub fn is_self_echo(&self, peer_id: &PeerId, client_id: &ClientId, filter: SelfFilter) -> bool {
        match filter {
            SelfFilter::IncludeAll => false,
            SelfFilter::ExcludeSameClient => &self.client_id == client_id,
            SelfFilter::ExcludeSamePeer => &self.peer_id == peer_id,
        }
    }
}

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

pub fn page_recent(room_id: RoomId, events: &[TranscriptEvent], limit: usize) -> TranscriptPage {
    let mut page_events = events.to_vec();
    page_events.sort_by(event_order);
    if page_events.len() > limit {
        page_events = page_events[page_events.len() - limit..].to_vec();
    }
    page_for(room_id, page_events)
}

pub fn page_before(
    room_id: RoomId,
    events: &[TranscriptEvent],
    before: &TranscriptCursor,
    limit: usize,
) -> TranscriptPage {
    let mut page_events: Vec<_> = events
        .iter()
        .filter(|event| cursor_before(&event.cursor(), before))
        .cloned()
        .collect();
    page_events.sort_by(event_order);
    if page_events.len() > limit {
        page_events = page_events[page_events.len() - limit..].to_vec();
    }
    page_for(room_id, page_events)
}

fn page_for(room_id: RoomId, events: Vec<TranscriptEvent>) -> TranscriptPage {
    TranscriptPage {
        room_id,
        newer: events.last().map(TranscriptEvent::cursor),
        older: events.first().map(TranscriptEvent::cursor),
        events,
    }
}

fn cursor_before(left: &TranscriptCursor, right: &TranscriptCursor) -> bool {
    left.lamport < right.lamport
        || (left.lamport == right.lamport && left.event_id.0 < right.event_id.0)
}

fn event_order(left: &TranscriptEvent, right: &TranscriptEvent) -> std::cmp::Ordering {
    left.lamport
        .cmp(&right.lamport)
        .then_with(|| left.event_id.0.cmp(&right.event_id.0))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            body: Some(format!("message {id}")),
            attachment: None,
            receipt: None,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn recent_page_is_ordered_and_cursor_backed() {
        let events = vec![
            event("e3", 3, "a", "a1"),
            event("e1", 1, "a", "a1"),
            event("e2", 2, "b", "b1"),
        ];

        let page = page_recent(RoomId("general".to_string()), &events, 2);

        assert_eq!(
            page.events
                .iter()
                .map(|e| &e.event_id.0)
                .collect::<Vec<_>>(),
            vec!["e2", "e3"]
        );
        assert_eq!(page.older.unwrap().event_id.0, "e2");
        assert_eq!(page.newer.unwrap().event_id.0, "e3");
    }

    #[test]
    fn older_page_uses_cursor_not_file_tail() {
        let events = vec![
            event("e1", 1, "a", "a1"),
            event("e2", 2, "b", "b1"),
            event("e3", 3, "c", "c1"),
            event("e4", 4, "d", "d1"),
        ];

        let page = page_before(
            RoomId("general".to_string()),
            &events,
            &TranscriptCursor {
                lamport: 4,
                event_id: EventId("e4".to_string()),
            },
            2,
        );

        assert_eq!(
            page.events
                .iter()
                .map(|e| &e.event_id.0)
                .collect::<Vec<_>>(),
            vec!["e2", "e3"]
        );
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

        let peer_filtered =
            filter_self_echoes(events, &peer, &current_client, SelfFilter::ExcludeSamePeer);
        assert_eq!(
            peer_filtered
                .iter()
                .map(|e| &e.event_id.0)
                .collect::<Vec<_>>(),
            vec!["other-peer"]
        );
    }

    #[test]
    fn attachment_manifest_is_machine_readable() {
        let manifest = AttachmentManifest {
            file_id: FileId("file-1".to_string()),
            name: "trace.json".to_string(),
            media_type: Some("application/json".to_string()),
            size_bytes: 42,
            content_hash: ContentHash("sha256:abc".to_string()),
            local_path: Some("/tmp/trace.json".to_string()),
            remote_ref: None,
        };

        let encoded = serde_json::to_value(&manifest).unwrap();

        assert_eq!(encoded["file_id"], "file-1");
        assert_eq!(encoded["content_hash"], "sha256:abc");
        assert_eq!(encoded["size_bytes"], 42);
    }
}
