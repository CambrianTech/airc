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

use airc_bus::DeliveryClass;
use airc_core::{Body, EventId, Headers, MentionTarget, RoomId};
use airc_protocol::headers_keys::HEADER_AIRC_DELIVERY_CLASS;
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
        self.publish_with_delivery(target, kind, body, headers, DeliveryClass::Durable)
            .await
    }

    /// [`publish`](Self::publish) with the DELIVERY CLASS chosen by the
    /// caller — the presence plane's publish.
    ///
    /// `publish` is durable because chat is durable: it becomes an ORM row
    /// and rides the replayable tail. But a citizen also emits lines that
    /// are STATE, not history — a glyph, a pose, "thinking" — and those must
    /// not accumulate in a transcript that recall, RAG, and every future
    /// digest then has to read past. Presence is state, not an event (#1341);
    /// this is the verb that says so at publish time.
    ///
    /// Two guarantees, and the second is the one that makes the first useful:
    ///
    /// 1. `EphemeralLatest` / `EphemeralWindow` never become ORM rows — the
    ///    daemon's durable sink never sees them.
    /// 2. The class is stamped into
    ///    [`HEADER_AIRC_DELIVERY_CLASS`] so a subscriber can filter on it
    ///    WITHOUT decoding the body. A working citizen drops the presence
    ///    plane by reading a header, not by parsing every thought behind it.
    ///
    /// Callers that want the class VISIBLE but the routing unchanged can
    /// keep using `publish`; nothing about the durable path moves here.
    pub async fn publish_with_delivery(
        &self,
        target: PublishTarget,
        kind: FrameKind,
        body: Body,
        mut headers: Headers,
        delivery: DeliveryClass,
    ) -> Result<PublishReceipt, AircError> {
        // Stamp before the split so BOTH paths (daemon-attached and the
        // direct send below) carry the class — a receiver must not have to
        // know which path a publisher happened to take.
        headers.insert(
            HEADER_AIRC_DELIVERY_CLASS.to_string(),
            delivery_class_header_value(delivery).to_string(),
        );
        let room = self.resolve_publish_target(&target).await?;
        if self.is_daemon_attached() {
            return self
                .daemon_publish(&room, kind, body, headers, delivery)
                .await;
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
                self.room_by_name_or_channel(name, "publish to").await
            }
        }
    }

    /// Resolve a room the caller NAMED — by channel name, or by the channel id
    /// it already holds — against this scope's subscription set.
    ///
    /// One resolver, every "this room or that one" surface. `publish` had this
    /// logic privately; the work board needs exactly the same question answered
    /// (continuum #345), and a second copy would be the kind of duplication that
    /// drifts — the two would disagree about whether an id is acceptable, or about
    /// what the refusal says.
    ///
    /// Accepts a channel ID as well as a name because callers legitimately hold
    /// either: a human types `#general`, while an agent reading a board or a
    /// receipt has the raw channel uuid and nothing else. Refusing the id would
    /// force every such caller to invent its own id→name lookup.
    ///
    /// NEVER auto-joins. A room outside the subscription set is a loud refusal
    /// naming the room and the remedy, because reading or publishing must not
    /// silently change what this scope is part of.
    pub async fn room_by_name_or_channel(
        &self,
        name_or_channel: &str,
        verb: &str,
    ) -> Result<crate::Room, AircError> {
        // Id first: a uuid is unambiguous, and a channel name can never parse as one.
        if let Ok(uuid) = uuid::Uuid::parse_str(name_or_channel) {
            let channel = RoomId::from_uuid(uuid);
            if let Some(room) = self.room_by_channel(channel).await? {
                return Ok(room);
            }
            return Err(AircError::Route(format!(
                "refusing to {verb} {name_or_channel:?}: this scope is not subscribed to that \
                 channel id. join the room first (this does not auto-join)."
            )));
        }
        let set = subscriptions::load_or_init(self.event_store()).await?;
        let channel_name = subscriptions::ChannelName::new(name_or_channel).map_err(|error| {
            AircError::Route(format!("channel name {name_or_channel:?}: {error}"))
        })?;
        set.subscribed
            .get(&channel_name)
            .map(|subscription| subscription.as_room())
            .ok_or_else(|| {
                AircError::Route(format!(
                    "refusing to {verb} {name_or_channel:?}: this scope is not subscribed to that \
                     channel. join the room first (this does not auto-join)."
                ))
            })
    }
}

/// `DeliveryClass` → the wire spelling carried by
/// [`HEADER_AIRC_DELIVERY_CLASS`]. Exhaustive on purpose: a new class must
/// choose its wire name here rather than defaulting into a wrong one, so a
/// future variant cannot silently masquerade as `durable` to every
/// header-filtering subscriber on the grid.
pub(crate) fn delivery_class_header_value(delivery: DeliveryClass) -> &'static str {
    match delivery {
        DeliveryClass::Durable => "durable",
        DeliveryClass::EphemeralLatest => "ephemeral_latest",
        DeliveryClass::EphemeralWindow => "ephemeral_window",
        DeliveryClass::RequestResponse => "request_response",
        DeliveryClass::StreamChunk => "stream_chunk",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the wire spelling of a delivery class going wrong
    // silently. Subscribers filter the presence plane by comparing this
    // header value; if a class ever renders as an unexpected string, every
    // header-filtering citizen on the grid mis-routes it and NOTHING errors
    // — the line just lands in the wrong plane. Pinning the exact bytes is
    // the only way that failure becomes loud.
    #[test]
    fn delivery_class_wire_names_are_exact_and_distinct() {
        use std::collections::HashSet;
        let all = [
            (DeliveryClass::Durable, "durable"),
            (DeliveryClass::EphemeralLatest, "ephemeral_latest"),
            (DeliveryClass::EphemeralWindow, "ephemeral_window"),
            (DeliveryClass::RequestResponse, "request_response"),
            (DeliveryClass::StreamChunk, "stream_chunk"),
        ];
        for (class, expected) in all {
            assert_eq!(
                delivery_class_header_value(class),
                expected,
                "the wire spelling of {class:?} is a cross-node contract"
            );
        }
        let distinct: HashSet<&str> = all.iter().map(|(_, name)| *name).collect();
        assert_eq!(
            distinct.len(),
            all.len(),
            "two classes sharing a wire name would make them indistinguishable to a              header-filtering subscriber"
        );
    }

    // what this catches: the presence plane being unreadable without a body
    // decode. The whole point of the class header is that a working citizen
    // drops presence lines by reading a header — if the constant drifts out
    // of the `airc.*` namespace or gets renamed, filters silently match
    // nothing and every presence line reaches the attention plane again.
    #[test]
    fn the_delivery_class_header_is_substrate_owned_and_named() {
        assert_eq!(HEADER_AIRC_DELIVERY_CLASS, "airc.delivery_class");
        assert!(
            HEADER_AIRC_DELIVERY_CLASS.starts_with("airc."),
            "routing headers live in the substrate-owned namespace"
        );
    }

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
