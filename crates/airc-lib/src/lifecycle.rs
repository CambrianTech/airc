//! Substrate-authored lifecycle events.
//!
//! Phase 2 of the GRID-SUBSTRATE-AUDIT. The substrate emits these
//! when state transitions happen (room joined, peer arrived, wire
//! established, etc.). Consumers subscribe via the existing
//! `Airc::subscribe`/`subscribe_subscribed_filtered` stream with
//! an `EventFilter` that includes the lifecycle kinds.
//!
//! Each variant has a stable body schema documented inline. The
//! schema is JSON because that's what the substrate's body format
//! already is — consumers parse via `serde_json::from_value` or
//! grab specific fields by key.
//!
//! These are **persisted** like any other transcript event so
//! consumers that reconnect after a disconnect can replay the
//! lifecycle history from their cursor. Filter them out at
//! subscription time if you don't want them in the chat stream.

use airc_core::{Body, ClientId, EventId, MentionTarget, PeerId, RoomId, TranscriptKind};
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

/// Body for `TranscriptKind::RoomJoined`. The local scope joined
/// this room (`channel_name` resolves to `room_id` on the local
/// wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomJoinedBody {
    pub channel_name: String,
    pub room_id: RoomId,
    pub wire: String,
    pub is_default: bool,
}

/// Body for `TranscriptKind::RoomParted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPartedBody {
    pub channel_name: String,
    pub room_id: RoomId,
}

/// Body for `TranscriptKind::PeerArrived`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerArrivedBody {
    pub peer_id: PeerId,
    /// How the peer entered local trust: `"invite"` /
    /// `"account_registry"` / `"manual"` / `"local_scope"`.
    pub via: String,
}

/// Body for `TranscriptKind::PeerDeparted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerDepartedBody {
    pub peer_id: PeerId,
    pub reason: String,
}

/// Body for `TranscriptKind::WireEstablished`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireEstablishedBody {
    pub wire: String,
    pub channel_name: String,
}

/// Body for `TranscriptKind::WireLost`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLostBody {
    pub wire: String,
    pub reason: String,
}

/// Body for `TranscriptKind::SubscriptionAdvanced`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionAdvancedBody {
    pub consumer_id: String,
    pub lamport: u64,
    pub event_id: EventId,
}

impl Airc {
    /// Emit a lifecycle event for the given room transition. Builds
    /// a [`TranscriptEvent`](airc_core::TranscriptEvent) signed
    /// from the local identity, persists it to the store, and fans
    /// it out to live subscribers. Lifecycle events are durable
    /// (cursor-replayable) so a consumer that reconnects sees the
    /// transitions it missed.
    ///
    /// Currently wired emit points:
    /// - `RoomJoined` — fired from [`Airc::join`] /
    ///   [`Airc::ensure_join_context`] after the subscription row
    ///   is persisted.
    /// - `PeerArrived` — fired from `Airc::add_peer` when a new peer
    ///   enters the local trust store.
    /// - `PeerDeparted` — fired from `Airc::remove_peer` when a peer
    ///   leaves local trust.
    /// - `WireEstablished` — fired when a new local wire subscriber
    ///   attaches.
    /// - `WireLost` — fired when the wire subscriber task exits,
    ///   either because the stream ended (`reason="stream_ended"`)
    ///   or because `Airc::teardown_wire` signalled it
    ///   (`reason="teardown"`).
    /// - `SubscriptionAdvanced` — fired when a runtime consumer cursor
    ///   advances, except for cursor advances caused by
    ///   `SubscriptionAdvanced` itself.
    /// - `RoomParted` — fired from [`Airc::part_channel`] after the
    ///   subscription row is tombstoned and presence is refreshed.
    ///
    /// All seven Phase 2 lifecycle variants are now wired.
    pub(crate) async fn emit_lifecycle(
        &self,
        kind: TranscriptKind,
        room_id: RoomId,
        body: Body,
    ) -> Result<(), AircError> {
        debug_assert!(
            kind.is_lifecycle(),
            "emit_lifecycle called with non-lifecycle kind {kind:?}",
        );
        let occurred_at_ms = now_ms()?;
        let lamport = self.next_lamport(occurred_at_ms);
        let event = airc_core::TranscriptEvent {
            event_id: EventId::new(),
            room_id,
            peer_id: self.inner.identity.peer_id,
            client_id: ClientId::new(),
            kind,
            occurred_at_ms,
            lamport,
            target: MentionTarget::All,
            headers: airc_core::headers::Headers::new(),
            body: Some(body),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        let event_id = event.event_id;
        let persist_result = self.inner.store.append(event.clone()).await;
        match persist_result {
            Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {
                if self.mark_broadcast(event_id) {
                    let _ = self.inner.live_tx.send(std::sync::Arc::new(event));
                }
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) async fn emit_subscription_advanced(
        &self,
        consumer_id: &str,
        cursor: &airc_core::TranscriptCursor,
        room_id: RoomId,
    ) -> Result<(), AircError> {
        let body = Body::Json(
            serde_json::to_value(SubscriptionAdvancedBody {
                consumer_id: consumer_id.to_string(),
                lamport: cursor.lamport,
                event_id: cursor.event_id,
            })
            .map_err(|e| AircError::Crypto(format!("lifecycle body serialize: {e}")))?,
        );
        self.emit_lifecycle(TranscriptKind::SubscriptionAdvanced, room_id, body)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_kinds_round_trip_through_is_lifecycle() {
        // All seven lifecycle variants must be classifiable as
        // lifecycle; the chat-shaped kinds must not.
        for kind in [
            TranscriptKind::PeerArrived,
            TranscriptKind::PeerDeparted,
            TranscriptKind::WireEstablished,
            TranscriptKind::WireLost,
            TranscriptKind::RoomJoined,
            TranscriptKind::RoomParted,
            TranscriptKind::SubscriptionAdvanced,
        ] {
            assert!(
                kind.is_lifecycle(),
                "{kind:?} should be classified as lifecycle"
            );
        }
        for kind in [
            TranscriptKind::Message,
            TranscriptKind::Attachment,
            TranscriptKind::Receipt,
            TranscriptKind::Presence,
            TranscriptKind::SessionControl,
            TranscriptKind::System,
        ] {
            assert!(!kind.is_lifecycle(), "{kind:?} should NOT be lifecycle");
        }
    }

    #[test]
    fn room_joined_body_round_trips_through_json() {
        let body = RoomJoinedBody {
            channel_name: "general".into(),
            room_id: RoomId::new(),
            wire: "/tmp/airc/wires/general".into(),
            is_default: true,
        };
        let json = serde_json::to_string(&body).unwrap();
        let parsed: RoomJoinedBody = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, body);
    }

    #[test]
    fn subscription_advanced_body_round_trips_through_json() {
        let body = SubscriptionAdvancedBody {
            consumer_id: "codex-hook:thread-1".to_string(),
            lamport: 42,
            event_id: EventId::from_u128(7),
        };
        let json = serde_json::to_string(&body).unwrap();
        let parsed: SubscriptionAdvancedBody = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, body);
    }

    #[test]
    fn room_parted_body_round_trips_through_json() {
        let body = RoomPartedBody {
            channel_name: "general".to_string(),
            room_id: RoomId::from_u128(0xabc),
        };
        let json = serde_json::to_string(&body).unwrap();
        let parsed: RoomPartedBody = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, body);
    }
}
