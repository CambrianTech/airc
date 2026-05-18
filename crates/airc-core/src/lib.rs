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

/// A peer's user-facing identity card.
///
/// The user/account concept in airc is identity-as-fields, not a
/// separate account-management subsystem. A "user" is identified by
/// the `name` (display nick) and matched canonically by the underlying
/// pubkey held in the peer's identity material (out of scope for this
/// type). The display fields here are what other peers see in
/// scrollback, presence, and `whois`.
///
/// Mirrors the Python `airc identity show` output exactly so the Rust
/// port doesn't redesign the shape — same six fields, same defaults.
/// Consumers (continuum, OpenClaw, etc.) may extend with their own
/// typed records keyed by pubkey; they don't extend `Identity` itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Identity {
    /// Display nick. Other peers see this in `from=` and `whois` output.
    pub name: String,
    /// Pronouns (e.g. "they", "she", "he"). Free-form short string.
    /// Default empty when unset.
    #[serde(default)]
    pub pronouns: String,
    /// One-tag role (e.g. "claude-arch", "device-link-coordinator",
    /// "human"). Free-form. Default empty.
    #[serde(default)]
    pub role: String,
    /// One-sentence bio describing what this identity does / focuses
    /// on. Free-form. Default empty.
    #[serde(default)]
    pub bio: String,
    /// IRC /away-style transient status. Cleared with empty string.
    /// Default empty (= not away).
    #[serde(default)]
    pub status: String,
    /// Short identity fingerprint derived from the peer's pubkey.
    /// Computed by airc identity tooling, not authored by the user.
    /// Format: short hex string matching the Python `airc identity
    /// show` `fingerprint:` line.
    #[serde(default)]
    pub fingerprint: String,
    /// Integration metadata for cross-system identity binding (e.g.
    /// GitHub login, Continuum persona id, OpenClaw user record). Map
    /// shape so consumers register their own keys without collision.
    /// airc never interprets the values; it just persists + transports.
    #[serde(default)]
    pub integrations: std::collections::BTreeMap<String, String>,
}

impl Identity {
    /// Construct an Identity with just a nick. All other fields default
    /// to empty/unset — same as a fresh `airc identity` with only the
    /// auto-derived nick filled in.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Is this identity "minimally set up" — has the user provided at
    /// least pronouns + role + bio? Used by the `airc identity` UX
    /// prompt to decide whether to nudge for completion. Mirrors the
    /// Python `_identity_needs_setup` heuristic.
    pub fn is_complete(&self) -> bool {
        !self.pronouns.is_empty() && !self.role.is_empty() && !self.bio.is_empty()
    }

    /// Mark / clear an "away" status. Empty string clears it (matches
    /// the Python `airc away ""` and `airc identity set --status ""`
    /// semantics).
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// Is this identity currently in an away/status-set state?
    pub fn is_away(&self) -> bool {
        !self.status.is_empty()
    }
}

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

    #[test]
    fn identity_new_sets_name_and_defaults_other_fields() {
        let id = Identity::new("claude-arch");
        assert_eq!(id.name, "claude-arch");
        assert_eq!(id.pronouns, "");
        assert_eq!(id.role, "");
        assert_eq!(id.bio, "");
        assert_eq!(id.status, "");
        assert_eq!(id.fingerprint, "");
        assert!(id.integrations.is_empty());
    }

    #[test]
    fn identity_is_complete_requires_pronouns_role_and_bio() {
        let mut id = Identity::new("alice");
        assert!(!id.is_complete(), "nick alone is not complete");

        id.pronouns = "they".into();
        assert!(!id.is_complete(), "pronouns alone is not complete");

        id.role = "architect".into();
        assert!(!id.is_complete(), "pronouns + role still missing bio");

        id.bio = "designs things".into();
        assert!(id.is_complete(), "all three set marks complete");
    }

    #[test]
    fn identity_away_status_lifecycle() {
        let mut id = Identity::new("alice");
        assert!(!id.is_away(), "default is not away");

        id.set_status("lunch");
        assert!(id.is_away());
        assert_eq!(id.status, "lunch");

        // Empty string clears — mirrors `airc away ""` semantics.
        id.set_status("");
        assert!(!id.is_away());
    }

    #[test]
    fn identity_serde_roundtrips_with_defaults_for_unset_fields() {
        // Forward-compat: an Identity stored in scope config.json when
        // only the nick was set should deserialize cleanly — other
        // fields default to empty rather than fail.
        let stored = serde_json::json!({
            "name": "bob",
        });
        let id: Identity = serde_json::from_value(stored).unwrap();
        assert_eq!(id.name, "bob");
        assert_eq!(id.pronouns, "");
        assert!(id.integrations.is_empty());

        // Round-trip a complete identity.
        let full = Identity {
            name: "claude-arch".into(),
            pronouns: "they".into(),
            role: "architect".into(),
            bio: "designs the airc-rust substrate".into(),
            status: "deep work".into(),
            fingerprint: "abcd1234".into(),
            integrations: {
                let mut m = std::collections::BTreeMap::new();
                m.insert("github".to_string(), "joelteply".to_string());
                m
            },
        };
        let encoded = serde_json::to_value(&full).unwrap();
        let decoded: Identity = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, full);
    }

    #[test]
    fn identity_integrations_map_namespace_collision_is_avoided() {
        // Consumers register their own keys; no airc-owned keys exist
        // in `integrations` because airc never interprets the values.
        // Two consumers can coexist.
        let mut id = Identity::new("multi-consumer");
        id.integrations
            .insert("github".to_string(), "joelteply".to_string());
        id.integrations
            .insert("continuum.persona_state_ref".to_string(), "uuid-abc".to_string());
        id.integrations
            .insert("openclaw.user_id".to_string(), "42".to_string());
        assert_eq!(id.integrations.len(), 3);
        // Order is deterministic (BTreeMap) so serde encoding is stable
        // — cursors / replay records / diffs work consistently.
        let encoded = serde_json::to_string(&id.integrations).unwrap();
        assert!(encoded.find("continuum").unwrap()
            < encoded.find("github").unwrap());
    }
}
