//! External-identity bridge contract.
//!
//! Closes work card fdc4b753 (P1, "External-identity bridge
//! contract: ExternalIdentity shape for non-PeerId chat
//! protocols").
//!
//! When AIRC bridges to Slack/Google Chat/Discord/etc., the posters
//! in those systems don't have native AIRC `PeerId`s. Today a bridge
//! has two bad options:
//!
//! 1. Sign every bridged message as the bridge process itself —
//!    loses per-user attribution.
//! 2. Mint synthetic `PeerId`s (e.g. UUIDv5 from username) —
//!    decouples from the cryptographic trust model; consumers can't
//!    distinguish "real AIRC peer" from "bridge-fabricated id."
//!
//! Both lie about the trust model.
//!
//! This module ships the typed primitive that lets bridges
//! preserve attribution honestly:
//!
//! - The wire-level frame is **signed by the bridge's own PeerId**
//!   — consumers can verify the bridge identity cryptographically.
//! - The frame body carries a typed [`ExternalIdentity`] describing
//!   the external user the bridge is *claiming on behalf of*.
//! - Headers `airc.bridge.source` / `airc.bridge.handle` let
//!   subscribers filter without decoding the body.
//!
//! So a Slack-bridged message reads as: "this event is
//! cryptographically signed by bridge X, who is claiming user Y on
//! Slack posted this content." That's an honest contract: the AIRC
//! trust model still tells consumers whom to trust (the bridge),
//! and the external identity is metadata.

use std::sync::Arc;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, TranscriptEvent};
use airc_protocol::FrameKind;
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

/// Header carrying the external-identity source. Filterable without
/// body decode.
pub const HEADER_BRIDGE_SOURCE: &str = "airc.bridge.source";
/// Header carrying the external-identity handle (the source's stable
/// user identifier).
pub const HEADER_BRIDGE_HANDLE: &str = "airc.bridge.handle";

/// Which external chat platform a bridge is relaying from.
///
/// `Other` is the escape hatch for platforms not in the closed set.
/// Closed variants exist for the common cases so dashboards can
/// pattern-match without parsing strings; new platforms add a
/// dedicated variant rather than living forever in `Other`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalIdentitySource {
    Slack,
    GoogleChat,
    Discord,
    MicrosoftTeams,
    /// Escape hatch — pass the platform identifier as a string.
    /// Existing well-known platforms should add a dedicated variant.
    Other(String),
}

impl ExternalIdentitySource {
    /// Stable string form used in headers + JSON. Matches the serde
    /// rename_all snake_case form for closed variants; `Other`
    /// emits the inner string directly.
    pub fn header_value(&self) -> String {
        match self {
            ExternalIdentitySource::Slack => "slack".to_string(),
            ExternalIdentitySource::GoogleChat => "google_chat".to_string(),
            ExternalIdentitySource::Discord => "discord".to_string(),
            ExternalIdentitySource::MicrosoftTeams => "microsoft_teams".to_string(),
            ExternalIdentitySource::Other(value) => value.clone(),
        }
    }
}

/// The external user a bridge is claiming attribution for. The
/// AIRC frame is signed by the bridge's PeerId; this struct is the
/// metadata describing who the bridge says posted on the source
/// platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalIdentity {
    pub source: ExternalIdentitySource,
    /// Stable user identifier on the source platform (Slack user
    /// ID, Discord user ID, Google Chat email, etc.). Used as the
    /// canonical id for cross-event correlation.
    pub handle: String,
    /// Human-readable name if known. Surface for UI; not used for
    /// identity comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// One typed bridged message. Wire frame is signed by the bridge's
/// PeerId; this struct is the body payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgedMessage {
    pub external_identity: ExternalIdentity,
    /// External system's channel / room / thread identifier (e.g.
    /// Slack channel ID `C012ABCD`). Optional because some platforms
    /// model DMs without a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_channel: Option<String>,
    pub text: String,
    pub posted_at_ms: u64,
}

/// Subscribe/query filter for bridged messages. `None` fields match
/// anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgedMessageFilter {
    /// Restrict to events whose `airc.bridge.source` header matches
    /// this source. Cheap header check; body never decoded if the
    /// source rules the event out.
    pub source: Option<ExternalIdentitySource>,
    /// Restrict to events whose `airc.bridge.handle` header matches
    /// this handle exactly.
    pub handle: Option<String>,
}

impl BridgedMessageFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_source(mut self, source: ExternalIdentitySource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn with_handle(mut self, handle: impl Into<String>) -> Self {
        self.handle = Some(handle.into());
        self
    }

    pub(crate) fn matches_transcript(&self, event: &TranscriptEvent) -> bool {
        if let Some(source) = self.source.as_ref() {
            let expected = source.header_value();
            match event.headers.get(HEADER_BRIDGE_SOURCE) {
                Some(value) if *value == expected => {}
                _ => return false,
            }
        }
        if let Some(handle) = self.handle.as_ref() {
            match event.headers.get(HEADER_BRIDGE_HANDLE) {
                Some(value) if value == handle => {}
                _ => return false,
            }
        }
        true
    }
}

impl Airc {
    /// Publish a bridged message. The wire frame is signed by the
    /// bridge's `PeerId` (i.e. this `Airc` handle); the body carries
    /// the typed `ExternalIdentity`. Consumers see "this frame is
    /// signed by the bridge, who is claiming user X on platform Y
    /// posted this content."
    pub async fn publish_bridged_message(
        &self,
        external_identity: ExternalIdentity,
        text: impl Into<String>,
        external_channel: Option<String>,
    ) -> Result<(), AircError> {
        let posted_at_ms = now_ms()?;
        let source_header = external_identity.source.header_value();
        let handle = external_identity.handle.clone();
        let message = BridgedMessage {
            external_identity,
            external_channel,
            text: text.into(),
            posted_at_ms,
        };
        let body = serde_json::to_value(&message)
            .map_err(|error| AircError::Crypto(format!("bridged message encode: {error}")))?;
        let mut headers = Headers::new();
        headers.insert(HEADER_BRIDGE_SOURCE.into(), source_header);
        headers.insert(HEADER_BRIDGE_HANDLE.into(), handle);
        self.send_frame_to(
            FrameKind::Message,
            MentionTarget::All,
            Body::Json(body),
            headers,
        )
        .await?;
        Ok(())
    }

    /// Live stream of typed bridged messages matching the filter.
    /// Bodies are only decoded when the header filter passes — so
    /// dashboards filtering by source pay near-zero cost on
    /// non-matching events.
    pub async fn subscribe_bridged_messages(
        &self,
        filter: BridgedMessageFilter,
    ) -> Result<impl Stream<Item = (Arc<TranscriptEvent>, BridgedMessage)>, AircError> {
        let inner = self.subscribe().await?;
        Ok(inner.filter_map(move |item| {
            let filter = filter.clone();
            async move {
                let event = item.ok()?;
                if !filter.matches_transcript(&event) {
                    return None;
                }
                let parsed = parse_bridged_message(&event)?;
                Some((event, parsed))
            }
        }))
    }

    /// Query recent bridged messages from the persisted transcript.
    pub async fn recent_bridged_messages(
        &self,
        filter: BridgedMessageFilter,
        window: usize,
    ) -> Result<Vec<BridgedMessage>, AircError> {
        let recent = self.page_recent(window).await?;
        let mut out = Vec::with_capacity(recent.len().min(window));
        for transcript_event in recent {
            if !filter.matches_transcript(&transcript_event) {
                continue;
            }
            if let Some(message) = parse_bridged_message(&transcript_event) {
                out.push(message);
            }
        }
        Ok(out)
    }
}

fn parse_bridged_message(event: &TranscriptEvent) -> Option<BridgedMessage> {
    let _ = event.headers.get(HEADER_BRIDGE_SOURCE)?;
    let body = event.body.as_ref()?;
    let value = match body {
        Body::Json(value) => value.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> ExternalIdentity {
        ExternalIdentity {
            source: ExternalIdentitySource::Slack,
            handle: "U012ABCD".to_string(),
            display_name: Some("Test User".to_string()),
        }
    }

    fn sample_message() -> BridgedMessage {
        BridgedMessage {
            external_identity: sample_identity(),
            external_channel: Some("C012ABCD".to_string()),
            text: "hello from slack".to_string(),
            posted_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn closed_source_header_values_are_snake_case() {
        assert_eq!(ExternalIdentitySource::Slack.header_value(), "slack");
        assert_eq!(
            ExternalIdentitySource::GoogleChat.header_value(),
            "google_chat"
        );
        assert_eq!(ExternalIdentitySource::Discord.header_value(), "discord");
        assert_eq!(
            ExternalIdentitySource::MicrosoftTeams.header_value(),
            "microsoft_teams"
        );
    }

    #[test]
    fn other_source_header_uses_inner_string() {
        assert_eq!(
            ExternalIdentitySource::Other("matrix".to_string()).header_value(),
            "matrix"
        );
    }

    #[test]
    fn external_identity_round_trips_through_json() {
        let identity = sample_identity();
        let json = serde_json::to_string(&identity).expect("encode");
        let decoded: ExternalIdentity = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, identity);
    }

    #[test]
    fn other_source_round_trips_through_json() {
        let identity = ExternalIdentity {
            source: ExternalIdentitySource::Other("matrix".to_string()),
            handle: "@user:example.org".to_string(),
            display_name: None,
        };
        let json = serde_json::to_string(&identity).expect("encode");
        let decoded: ExternalIdentity = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, identity);
    }

    #[test]
    fn bridged_message_round_trips_through_json() {
        let message = sample_message();
        let json = serde_json::to_string(&message).expect("encode");
        let decoded: BridgedMessage = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, message);
    }

    #[test]
    fn display_name_is_optional_in_json() {
        let identity = ExternalIdentity {
            source: ExternalIdentitySource::Discord,
            handle: "123456".to_string(),
            display_name: None,
        };
        let json = serde_json::to_string(&identity).expect("encode");
        assert!(!json.contains("display_name"));
        let decoded: ExternalIdentity = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.display_name, None);
    }

    #[test]
    fn external_channel_is_optional_in_json() {
        let message = BridgedMessage {
            external_identity: sample_identity(),
            external_channel: None,
            text: "dm content".to_string(),
            posted_at_ms: 0,
        };
        let json = serde_json::to_string(&message).expect("encode");
        assert!(!json.contains("external_channel"));
    }

    #[test]
    fn filter_builder_chains() {
        let filter = BridgedMessageFilter::new()
            .with_source(ExternalIdentitySource::Slack)
            .with_handle("U012ABCD");
        assert!(matches!(filter.source, Some(ExternalIdentitySource::Slack)));
        assert_eq!(filter.handle.as_deref(), Some("U012ABCD"));
    }

    #[test]
    fn empty_filter_matches_anything_with_bridge_source_header() {
        let filter = BridgedMessageFilter::default();
        let mut headers = airc_core::headers::Headers::new();
        headers.insert(HEADER_BRIDGE_SOURCE.into(), "slack".to_string());
        let event = TranscriptEvent {
            event_id: airc_core::EventId::new(),
            peer_id: airc_core::PeerId::new(),
            client_id: airc_core::ClientId::new(),
            room_id: airc_core::RoomId::new(),
            kind: airc_core::TranscriptKind::Message,
            occurred_at_ms: 0,
            lamport: 0,
            target: airc_core::transcript::MentionTarget::All,
            headers,
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        assert!(filter.matches_transcript(&event));
    }
}
