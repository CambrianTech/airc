//! Inbound-frame delivery into the daemon's owner-core router
//! (card 4132f48c — the store-split fix).
//!
//! ## The split this closes
//!
//! The daemon process owns embedded `Airc` handles for its LAN
//! listener (account-registry glue) and its outbound route-discovery
//! dials. Before this module, an inbound LAN frame was ingested by
//! `append_received_frame` into that handle's **SDK store** (the
//! `events` table of `~/.airc/events.sqlite`) and fanned out only on
//! that handle's private in-process broadcast. Nothing reached the
//! daemon's [`EventRouter`] — but the router IS the transcript every
//! operator scope actually reads: `airc inbox` / `airc msg` /
//! monitors attach to the machine-singular daemon and page or stream
//! the router's hot ring + durable tier (`bus_events`). Result: the
//! frame was durable, acked (post-#1155), and invisible to every
//! operator surface.
//!
//! ## The mechanism (mirrors local delivery)
//!
//! A local `airc msg` propagates across scopes on one machine by ONE
//! mechanism: `Request::Send`/`Publish` → `router.publish` → hot ring,
//! live fan-out to attached scopes, write-behind to the machine
//! ORM. This module gives inbound transport frames the SAME path:
//! the daemon installs a [`RouterInboundBridge`] as the
//! [`InboundFrameSink`] of its transport-owning handles, and
//! `append_received_frame` hands every received frame to the bridge
//! INSTEAD of the handle's scope store. Fan-out at delivery, no
//! per-scope copies, one durable transcript per machine (§3.3).
//!
//! ## No double delivery
//!
//! The router's [`EventRouter::publish_if_new`] is idempotent on the
//! sender-minted `event_id`: the same frame arriving on two LAN links
//! (listener + dialer handles), or a wire echo of an event a local
//! scope already published through the router, publishes exactly
//! once. Cursor coherence is free: bridged events receive owner-
//! assigned `(epoch, counter)` seqs exactly like local publishes, so
//! subscription cursors stay monotonic per the router's total order.
//!
//! ## Truthful delivery acks (extends card 39d37629)
//!
//! With a sink installed, "delivered" means: published into the
//! router (visible to every scope subscribed to the channel) AND at
//! least one scope on this machine has the channel bound (presence
//! beacons in the coordinator store). A frame on a channel no scope
//! binds is still published durably (no data loss; a later subscriber
//! replays it) but is acked `undeliverable{unknown_channel}` and
//! diagnosed loudly — exactly the #1155 posture, upgraded from
//! "bound in the receiving handle's scope" to "bound by any scope the
//! machine serves".

use std::sync::Arc;

use airc_bus::envelope::{DeliveryClass, Kind, Target};
use airc_bus::{EventRouter, PublishIfNew};
use airc_core::transcript::MentionTarget;
use airc_core::RoomId;
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_protocol::{Frame, FrameKind};
use airc_store::EventStore;
use async_trait::async_trait;

use crate::coordinator::{self, CoordinatorConfig};
use crate::subscriptions::derive_room_id;
use crate::time;

/// Verdict of an [`InboundFrameSink::deliver`] call. This is the
/// persistence decision the delivery ack (card 39d37629) reports when
/// a sink owns inbound delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundDeliveryVerdict {
    /// The frame is (or already was) in the machine transcript every
    /// subscribed scope reads, and at least one scope binds the
    /// channel.
    Delivered,
    /// Durably stored, but no scope on this machine has the frame's
    /// channel bound — no transcript surface will show it until one
    /// does. Reported as `undeliverable{unknown_channel}`.
    UnknownChannel,
    /// The frame could not be made durable/visible. Reported as
    /// `undeliverable{persist_failed}`.
    Failed(String),
}

/// Where a daemon host routes inbound transport frames. When a sink
/// is installed on an [`crate::Airc`] handle
/// ([`crate::Airc::set_inbound_frame_sink`]), the transport ingest
/// hands every received frame here INSTEAD of appending it to the
/// handle's scope store; the verdict is the persistence decision the
/// delivery ack reports.
#[async_trait]
pub trait InboundFrameSink: Send + Sync {
    async fn deliver(&self, frame: &Frame) -> InboundDeliveryVerdict;
}

/// The production sink: delivers inbound frames into the daemon's
/// [`EventRouter`] — the same fan-out + durable tier local sends use.
pub struct RouterInboundBridge {
    router: EventRouter,
    coordinator_store: Arc<dyn EventStore>,
    diag_sink: Arc<dyn DiagnosticSink>,
}

impl RouterInboundBridge {
    /// `router` and `coordinator_store` are the daemon's own (the
    /// owner-core engine and the machine coordinator store its
    /// presence beacons live in).
    pub fn new(router: EventRouter, coordinator_store: Arc<dyn EventStore>) -> Self {
        Self {
            router,
            coordinator_store,
            diag_sink: Arc::new(StderrJsonDiagnosticSink),
        }
    }

    /// Replace the diagnostic sink (tests assert emissions instead of
    /// scraping stderr — same pattern as `Airc::set_diagnostic_sink`).
    #[must_use]
    pub fn with_diagnostic_sink(mut self, sink: Arc<dyn DiagnosticSink>) -> Self {
        self.diag_sink = sink;
        self
    }

    /// Does any scope on this machine bind `channel`? Source of truth:
    /// presence beacons in the coordinator store — every scope's
    /// `join`/`ensure_join_context` publishes its subscribed channel
    /// names there. Stale beacons count: a subscription is durable
    /// scope state, and a quiet scope still reads its transcript later.
    async fn channel_has_subscribed_scope(&self, channel: RoomId) -> Result<bool, String> {
        let cached = crate::mesh_identity::resolve(self.coordinator_store.as_ref())
            .await
            .map_err(|e| format!("mesh identity: {e}"))?;
        let identity = cached.as_mesh_identity();
        let now_ms = time::now_ms().map_err(|e| format!("clock: {e}"))?;
        let snapshot = coordinator::snapshot_store(
            self.coordinator_store.as_ref(),
            &identity,
            &CoordinatorConfig::default(),
            now_ms,
        )
        .await
        .map_err(|e| format!("beacon snapshot: {e}"))?;
        Ok(snapshot
            .live
            .iter()
            .chain(snapshot.stale.iter())
            .flat_map(|beacon| beacon.subscribed_channels.iter())
            .any(|name| derive_room_id(&identity, name) == channel))
    }
}

#[async_trait]
impl InboundFrameSink for RouterInboundBridge {
    async fn deliver(&self, frame: &Frame) -> InboundDeliveryVerdict {
        let env = bus_envelope_for_inbound(frame);
        let event_id = frame.envelope.event_id;
        let channel = frame.envelope.channel;
        // Card 1998f6cb: attach the verified link origin so the
        // router's outbound forward sink (when installed) never
        // echoes this frame back over the link it arrived on.
        let origin = crate::transport::link_origin(frame);
        match self.router.publish_if_new_from(env, Some(origin)).await {
            // Duplicate IS delivered: the existing copy stands, so the
            // ack stays truthful and the second LAN link's copy never
            // double-fans-out.
            Ok(PublishIfNew::Published(_)) | Ok(PublishIfNew::Duplicate) => {
                match self.channel_has_subscribed_scope(channel).await {
                    Ok(true) => InboundDeliveryVerdict::Delivered,
                    Ok(false) => InboundDeliveryVerdict::UnknownChannel,
                    Err(error) => {
                        // Can't read the beacon set ⇒ can't honestly
                        // claim a subscribed scope will see it. Loud,
                        // then the unknown-channel verdict (the frame
                        // IS durable in the router — `persist_failed`
                        // would be the lie here).
                        self.diag_sink.emit(
                            DiagnosticEvent::error(
                                DiagnosticComponent::Subscriber,
                                DiagnosticCode::FrameUndeliverable,
                                "beacon set unreadable while concluding inbound delivery",
                            )
                            .with_field("event_id", event_id)
                            .with_field("channel", channel)
                            .with_field("error", error),
                        );
                        InboundDeliveryVerdict::UnknownChannel
                    }
                }
            }
            Err(error) => InboundDeliveryVerdict::Failed(format!("router publish: {error}")),
        }
    }
}

/// Project an inbound transport [`Frame`] onto the owner-core envelope
/// shape — the exact inverse pairing of the daemon's IPC publish path
/// (`airc-daemon::handlers::publish_envelope` + the SDK's
/// `daemon_send_frame`), so a bridged LAN frame and a locally published
/// frame of the same content are indistinguishable to readers.
///
/// The sender-minted `event_id` is preserved: it is the dedup key in
/// [`EventRouter::publish_if_new`] and the identity the delivery ack
/// (`ack.for_event`) refers to. `seq`/`occurred_at_ms` are owner-
/// stamped at publish, exactly like local sends.
fn bus_envelope_for_inbound(frame: &Frame) -> airc_bus::envelope::Envelope {
    let payload = frame
        .envelope
        .body
        .as_ref()
        .map(airc_core::Body::to_payload)
        .unwrap_or_default();
    let kind = match frame.kind {
        FrameKind::Message => Kind::Message,
        FrameKind::Event => Kind::Event,
        FrameKind::Control => Kind::Control,
    };
    let mut env = airc_bus::envelope::Envelope::new(
        frame.envelope.channel,
        (frame.envelope.sender, frame.envelope.sender_client),
        kind,
        DeliveryClass::Durable,
        bytes::Bytes::from(payload),
    );
    env.event_id = frame.envelope.event_id;
    // Mirrors `From<MentionTarget> for IpcTarget` (airc-ipc
    // sdk_conversions): room mentions round-trip as `room:<uuid>`
    // endpoints, which `target_to_mention` parses back.
    env.target = match frame.envelope.target {
        MentionTarget::All => Target::All,
        MentionTarget::Peer(peer) => Target::Peer(peer),
        MentionTarget::Room(room) => Target::Endpoint(format!("room:{}", room.as_uuid())),
    };
    env.correlation_id = frame.envelope.reply_to.map(|id| id.as_uuid());
    env.headers = frame.envelope.headers.clone();
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{Body, EventId, Headers, PeerId};
    use airc_protocol::{Envelope as ProtoEnvelope, Signature};

    fn frame(channel: RoomId, event_id: EventId) -> Frame {
        Frame {
            kind: FrameKind::Message,
            envelope: ProtoEnvelope {
                event_id,
                sender: PeerId::new(),
                sender_client: airc_core::ClientId::new(),
                channel,
                target: MentionTarget::All,
                lamport: 7,
                occurred_at_ms: 1_000,
                reply_to: None,
                headers: Headers::new(),
                body: Some(Body::text("bridged")),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    #[test]
    fn inbound_projection_preserves_identity_and_payload() {
        let channel = RoomId::new();
        let event_id = EventId::new();
        let f = frame(channel, event_id);
        let env = bus_envelope_for_inbound(&f);
        assert_eq!(env.event_id, event_id, "sender-minted id must survive");
        assert_eq!(env.channel, channel);
        assert_eq!(env.kind, Kind::Message);
        assert_eq!(env.delivery, DeliveryClass::Durable);
        let body = Body::from_payload(&env.payload).expect("payload round-trips");
        assert_eq!(body, Body::text("bridged"));
    }
}
