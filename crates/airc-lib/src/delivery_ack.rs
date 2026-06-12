//! Delivery-ack send + respond surface (card 39d37629).
//!
//! The receiving half lives in `transport.rs::append_received_frame`
//! (interception of ack responses, ack emission after the persistence
//! decision); this module owns:
//!
//!   - [`Airc::send_with_delivery_ack`] — the sender verb: request an
//!     ack on an ordinary send, wait (bounded) for the typed receipt;
//!   - [`Airc::conclude_delivery_ack`] / [`Airc::respond_delivery_ack`]
//!     — the receiver-side helpers that decide delivered vs
//!     undeliverable AFTER persistence and write the signed ack back
//!     over the point-to-point LAN connection.
//!
//! Routed daemon sends are untouched: nothing here runs unless the
//! inbound frame carries the `airc.delivery_ack: request` header,
//! which only the ack-requesting verb sets.

use std::time::Duration;

use airc_core::{Body, EventId, Headers, PeerId, RoomId, TranscriptCursor};
use airc_diagnostics::{DiagnosticCode, DiagnosticComponent, DiagnosticEvent};
use airc_protocol::{
    DeliveryAck, DeliveryOutcome, Envelope, Frame, FrameKind, Signature, UndeliverableReason,
    DELIVERY_ACK_REQUEST, DELIVERY_ACK_RESPONSE, HEADER_AIRC_DELIVERY_ACK,
};

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

/// Typed outcome of an ack-requesting send, as seen by the SENDER.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliverySendOutcome {
    /// The receiver persisted the frame into a bound room transcript
    /// and said so. `ack.outcome` carries the channel + cursor.
    Delivered { event_id: EventId, ack: DeliveryAck },
    /// The receiver accepted the frame but reported it undeliverable
    /// (unknown channel, persist failure, ...).
    Undeliverable { event_id: EventId, ack: DeliveryAck },
    /// No ack arrived within the timeout. Distinct from undeliverable:
    /// the receiver may be running an older build (which never acks)
    /// or may have dropped the frame — the sender cannot tell, and
    /// MUST NOT claim delivery.
    NoAck { event_id: EventId, waited: Duration },
}

impl Airc {
    /// Send `text` to the current room requesting a delivery ack, and
    /// wait up to `timeout` for the receiver's typed receipt.
    ///
    /// This is the `lan-send` verb's contract fix (card 39d37629):
    /// "sent" used to mean "bytes flushed to the TLS socket", which a
    /// receiver could accept and then silently lose before transcript
    /// persistence (live repro 2026-06-12 02:36Z). With this verb the
    /// sender's outcome is `Delivered` only when the receiver persisted
    /// the frame on a bound channel — anything else is loud and typed.
    ///
    /// Scope: the point-to-point verb path (a direct `Airc::open`
    /// handle with a live `connect_lan` link, as `lan-send` builds).
    /// On a daemon-attached handle the ingest — and therefore the ack
    /// interception — happens in the daemon process, so this method
    /// would always report `NoAck`; routed-ack is a follow-up card.
    pub async fn send_with_delivery_ack(
        &self,
        text: &str,
        mut headers: Headers,
        timeout: Duration,
    ) -> Result<DeliverySendOutcome, AircError> {
        // Subscribe BEFORE sending so an ack racing back faster than
        // this task resumes cannot be missed (broadcast receivers see
        // only what is sent after they exist).
        let mut ack_rx = self.inner.ack_tx.subscribe();
        headers.insert(
            HEADER_AIRC_DELIVERY_ACK.to_string(),
            DELIVERY_ACK_REQUEST.to_string(),
        );
        let event_id = self.send(Body::text(text), headers).await?;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let ack = match tokio::time::timeout_at(deadline, ack_rx.recv()).await {
                Err(_elapsed) => {
                    return Ok(DeliverySendOutcome::NoAck {
                        event_id,
                        waited: timeout,
                    });
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    return Ok(DeliverySendOutcome::NoAck {
                        event_id,
                        waited: timeout,
                    });
                }
                Ok(Ok(ack)) => ack,
            };
            if ack.for_event != event_id {
                continue;
            }
            return Ok(match ack.outcome {
                DeliveryOutcome::Delivered { .. } => {
                    DeliverySendOutcome::Delivered { event_id, ack }
                }
                DeliveryOutcome::Undeliverable { .. } => {
                    DeliverySendOutcome::Undeliverable { event_id, ack }
                }
            });
        }
    }

    /// Receiver side, after a successful persist: decide delivered vs
    /// unknown-channel and send the receipt. "Delivered" requires the
    /// frame's channel to be BOUND in this scope's subscription set —
    /// a frame persisted onto a channel no local transcript surface
    /// reads is exactly the invisible-delivery failure the live repro
    /// hit, so it is reported as undeliverable{unknown_channel} (the
    /// event itself stays persisted in case the channel is bound
    /// later; the diagnostic says so).
    pub(crate) async fn conclude_delivery_ack(
        &self,
        origin: PeerId,
        for_event: EventId,
        channel: RoomId,
        cursor: TranscriptCursor,
    ) {
        let bound = match self.subscription_set().await {
            Ok(set) => set.all().any(|sub| sub.room_id == channel),
            Err(error) => {
                // Can't read the subscription set ⇒ can't honestly
                // claim the channel is bound. Loud + undeliverable.
                self.emit_diag(
                    DiagnosticEvent::error(
                        DiagnosticComponent::Subscriber,
                        DiagnosticCode::FrameUndeliverable,
                        "subscription set unreadable while concluding delivery ack",
                    )
                    .with_field("reason", UndeliverableReason::UnknownChannel.as_str())
                    .with_field("event_id", for_event)
                    .with_field("error", error),
                );
                false
            }
        };
        if bound {
            self.respond_delivery_ack(
                origin,
                for_event,
                channel,
                DeliveryOutcome::Delivered { channel, cursor },
            )
            .await;
        } else {
            self.emit_diag(
                DiagnosticEvent::error(
                    DiagnosticComponent::Subscriber,
                    DiagnosticCode::FrameUndeliverable,
                    "accepted frame addressed to a channel this scope has not bound — \
                     persisted but no local transcript surface will show it",
                )
                .with_field("reason", UndeliverableReason::UnknownChannel.as_str())
                .with_field("event_id", for_event)
                .with_field("sender", origin)
                .with_field("channel", channel)
                .with_field("persisted", true),
            );
            self.respond_delivery_ack(
                origin,
                for_event,
                channel,
                DeliveryOutcome::Undeliverable {
                    reason: UndeliverableReason::UnknownChannel,
                },
            )
            .await;
        }
    }

    /// Build, sign, and unicast a delivery-ack response back to the
    /// requesting sender over the LAN connection the frame arrived on
    /// (point-to-point verb path). Failure to send the ack is itself
    /// loud — the frame's fate was already decided and logged; the
    /// sender will see an ack timeout instead of the typed outcome.
    pub(crate) async fn respond_delivery_ack(
        &self,
        origin: PeerId,
        for_event: EventId,
        channel: RoomId,
        outcome: DeliveryOutcome,
    ) {
        let ack = DeliveryAck {
            for_event,
            receiver: self.inner.identity.peer_id,
            outcome,
        };
        let body_json = match serde_json::to_value(&ack) {
            Ok(value) => value,
            Err(error) => {
                self.emit_diag(
                    DiagnosticEvent::error(
                        DiagnosticComponent::Subscriber,
                        DiagnosticCode::DeliveryAckSendFailed,
                        "delivery ack body did not encode",
                    )
                    .with_field("for_event", for_event)
                    .with_field("error", error),
                );
                return;
            }
        };
        let occurred_at_ms = match now_ms() {
            Ok(ms) => ms,
            Err(error) => {
                self.emit_diag(
                    DiagnosticEvent::error(
                        DiagnosticComponent::Subscriber,
                        DiagnosticCode::DeliveryAckSendFailed,
                        "clock unavailable while building delivery ack",
                    )
                    .with_field("for_event", for_event)
                    .with_field("error", error),
                );
                return;
            }
        };
        let mut headers = Headers::new();
        headers.insert(
            HEADER_AIRC_DELIVERY_ACK.to_string(),
            DELIVERY_ACK_RESPONSE.to_string(),
        );
        let lamport = self.next_lamport(occurred_at_ms);
        let mut frame = Frame {
            kind: FrameKind::Control,
            envelope: Envelope {
                event_id: EventId::new(),
                sender: self.inner.identity.peer_id,
                sender_client: self.inner.identity.client_id,
                channel,
                target: airc_core::transcript::MentionTarget::Peer(origin),
                lamport,
                occurred_at_ms,
                reply_to: None,
                headers,
                body: Some(Body::Json(body_json)),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        };
        frame.envelope.signature = match self.inner.identity.keypair.sign_envelope(
            &frame.envelope,
            self.inner.identity.peer_id,
            0,
        ) {
            Ok(signature) => signature,
            Err(error) => {
                self.emit_diag(
                    DiagnosticEvent::error(
                        DiagnosticComponent::Subscriber,
                        DiagnosticCode::DeliveryAckSendFailed,
                        "delivery ack envelope did not sign",
                    )
                    .with_field("for_event", for_event)
                    .with_field("error", error),
                );
                return;
            }
        };
        let adapter = match self.lan_adapter().await {
            Ok(adapter) => adapter,
            Err(error) => {
                self.emit_diag(
                    DiagnosticEvent::warn(
                        DiagnosticComponent::Subscriber,
                        DiagnosticCode::DeliveryAckSendFailed,
                        "no LAN adapter available to return delivery ack",
                    )
                    .with_field("for_event", for_event)
                    .with_field("origin", origin)
                    .with_field("error", error),
                );
                return;
            }
        };
        if let Err(error) = adapter.send_to(origin, frame).await {
            self.emit_diag(
                DiagnosticEvent::warn(
                    DiagnosticComponent::Subscriber,
                    DiagnosticCode::DeliveryAckSendFailed,
                    "delivery ack could not be written back to the sender",
                )
                .with_field("for_event", for_event)
                .with_field("origin", origin)
                .with_field("error", error),
            );
        }
    }
}
