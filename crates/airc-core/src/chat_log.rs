//! Text-chat log envelope used by the current shell boundary.
//!
//! This is not a transport adapter. It is the small typed record that the
//! shell wrapper still appends to `messages.jsonl` while the Rust event bus
//! takes over. Keeping the shape here prevents command modules, monitors,
//! and adapters from each hand-rolling the same JSON.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ChatLogEnvelope<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub ts: &'a str,
    pub channel: &'a str,
    pub msg: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub client_id: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub kind: &'a str,
}

impl<'a> ChatLogEnvelope<'a> {
    pub fn new(from: &'a str, to: &'a str, ts: &'a str, channel: &'a str, msg: &'a str) -> Self {
        Self {
            from,
            to,
            ts,
            channel,
            msg,
            client_id: "",
            kind: "",
        }
    }

    pub fn with_client_id(mut self, client_id: &'a str) -> Self {
        self.client_id = client_id;
        self
    }

    pub fn with_kind(mut self, kind: &'a str) -> Self {
        self.kind = kind;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_required_chat_fields() {
        let envelope = ChatLogEnvelope::new(
            "alice",
            "all",
            "2026-05-19T00:00:00Z",
            "general",
            "line\nquote \" slash \\",
        )
        .with_client_id("client")
        .with_kind("heartbeat");

        let encoded = serde_json::to_value(envelope).unwrap();
        assert_eq!(encoded["from"], "alice");
        assert_eq!(encoded["to"], "all");
        assert_eq!(encoded["channel"], "general");
        assert_eq!(encoded["msg"], "line\nquote \" slash \\");
        assert_eq!(encoded["client_id"], "client");
        assert_eq!(encoded["kind"], "heartbeat");
    }

    #[test]
    fn omits_empty_optional_fields() {
        let envelope =
            ChatLogEnvelope::new("alice", "all", "2026-05-19T00:00:00Z", "general", "hi");

        let encoded = serde_json::to_value(envelope).unwrap();
        assert!(encoded.get("client_id").is_none());
        assert!(encoded.get("kind").is_none());
    }
}
