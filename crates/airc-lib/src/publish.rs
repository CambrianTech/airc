//! Structured publish API for AIRC consumers (Continuum chat,
//! OpenClaw, Hermes, etc.).
//!
//! Work card a0d740fa (P1): "Structured AIRC publish API for
//! Continuum chat dual-write". This module gives consumers one
//! typed publish path for opaque bodies + headers, with a receipt
//! carrying event id, lamport, and channel.
//!
//! This module ships the substrate-level publish primitive:
//!
//! - [`PublishTarget`] — typed routing target. `CurrentRoom` keeps
//!   the existing behaviour; `RoomByName(...)` routes to an
//!   already-subscribed room without touching the default pointer.
//! - [`PublishReceipt`] — typed receipt: event id, lamport,
//!   occurred-at, channel id, channel name. JSON-serialisable, so
//!   the CLI can emit it verbatim for shell consumers.
//! - [`Airc::publish`] — the API.
//!
//! Layering: native substrate owns the truth (frame construction +
//! routing + receipt). SDK consumers compose it idiomatically. The
//! CLI is a thin pass-through over the same call.
//!
//! The same call works for in-process [`Airc::open`] handles and
//! daemon-attached [`Airc::attach`] handles.

use airc_core::{Body, EventId, Headers, MentionTarget, RoomId};
use airc_protocol::FrameKind;
use serde::{Deserialize, Serialize};

use crate::error::AircError;
use crate::subscriptions;
use crate::Airc;

/// Where a [`PublishReceipt`] should land.
///
/// `CurrentRoom` routes to whatever this scope considers default.
/// `RoomByName(name)` requires the channel name to be in this
/// scope's subscription set and routes to that room directly; it
/// does NOT auto-join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishTarget {
    /// Route to this scope's default subscribed room.
    CurrentRoom,
    /// Route to a specific room by channel name. The room must
    /// already be in this scope's subscription set; refusing to
    /// auto-join is intentional — publishing should not change
    /// what rooms this scope is part of.
    RoomByName(String),
}

/// Typed receipt returned by [`Airc::publish`]. JSON-serialisable
/// so the CLI can pass it through to shell consumers without
/// human-prose parsing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishReceipt {
    /// AIRC-assigned event id for this publish.
    pub event_id: EventId,
    /// Lamport counter at publish time (substrate ordering).
    pub lamport: u64,
    /// UNIX epoch millis recorded when the frame was constructed.
    pub occurred_at_ms: u64,
    /// Channel UUID the frame was routed to.
    pub channel_id: RoomId,
    /// Channel name the frame was routed to.
    pub channel_name: String,
}

impl Airc {
    /// Publish a typed body to a specific room without touching
    /// this scope's default-room pointer.
    ///
    /// `kind` selects the frame kind:
    /// - [`FrameKind::Message`] for human-readable chat.
    /// - [`FrameKind::Event`] for structured envelopes
    ///   (recommended for Continuum-style consumers that carry a
    ///   typed body + filterable headers).
    /// - [`FrameKind::Control`] for control-plane signalling.
    ///
    /// Returns a typed [`PublishReceipt`] carrying the event id,
    /// lamport, and concrete channel — no stdout parsing required.
    pub async fn publish(
        &self,
        target: PublishTarget,
        kind: FrameKind,
        body: Body,
        headers: Headers,
    ) -> Result<PublishReceipt, AircError> {
        let room = self.resolve_publish_target(&target).await?;
        if self.is_daemon_attached() {
            return self.daemon_publish(&room, kind, body, headers).await;
        }
        let result = self
            .send_frame_to_room(kind, MentionTarget::All, body, headers, &room)
            .await?;
        Ok(PublishReceipt {
            event_id: result.event_id,
            lamport: result.lamport,
            occurred_at_ms: result.occurred_at_ms,
            channel_id: room.channel,
            channel_name: room.name,
        })
    }

    async fn resolve_publish_target(
        &self,
        target: &PublishTarget,
    ) -> Result<crate::Room, AircError> {
        match target {
            PublishTarget::CurrentRoom => self.current_room().await,
            PublishTarget::RoomByName(name) => {
                let set = subscriptions::load_or_init(self.event_store()).await?;
                let channel_name = subscriptions::ChannelName::new(name).map_err(|error| {
                    AircError::Route(format!("publish target channel name {name:?}: {error}"))
                })?;
                set.subscribed
                    .get(&channel_name)
                    .map(|subscription| subscription.as_room())
                    .ok_or_else(|| {
                        AircError::Route(format!(
                            "refusing to publish to {name:?}: this scope is not subscribed to that \
                             channel. join the room first (publish does not auto-join)."
                        ))
                    })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_target_round_trips_through_clone_and_equality() {
        let a = PublishTarget::CurrentRoom;
        let b = PublishTarget::RoomByName("project-x".into());
        assert_eq!(a, a.clone());
        assert_eq!(b, b.clone());
        assert_ne!(a, b);
    }

    #[test]
    fn publish_receipt_serializes_to_stable_snake_case_json() {
        let receipt = PublishReceipt {
            event_id: EventId::from_uuid(uuid::Uuid::nil()),
            lamport: 42,
            occurred_at_ms: 1_700_000_000_000,
            channel_id: RoomId::from_uuid(uuid::Uuid::nil()),
            channel_name: "project-x".to_string(),
        };
        let value = serde_json::to_value(&receipt).expect("encode");
        assert_eq!(value["lamport"], 42);
        assert_eq!(value["occurred_at_ms"], 1_700_000_000_000_u64);
        assert_eq!(value["channel_name"], "project-x");
        // Round-trip
        let decoded: PublishReceipt = serde_json::from_value(value).expect("decode");
        assert_eq!(decoded, receipt);
    }
}
