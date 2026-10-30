//! The receiving end: every inbound `FrameKind::Addressed` frame comes here
//! before anything could store it, and nothing here stores it.

use std::sync::Arc;

use airc_core::MentionTarget;
use airc_protocol::addressed::{aad, addressed_channel, AddressedWire, SealedInner};
use airc_protocol::{Frame, SessionManagerError};

use super::VerifiedAddressed;
use crate::time::now_ms;
use crate::Airc;

impl Airc {
    /// Take one inbound addressed frame. A frame for another identity is a
    /// third party's: it is dropped unread. A frame for this identity is a
    /// handshake step (answered) or a sealed payload (opened into the inbox).
    pub(crate) async fn receive_addressed(&self, frame: Frame) {
        let me = self.peer_id();
        let envelope = frame.envelope;
        if envelope.target != MentionTarget::Peer(me) || envelope.channel != addressed_channel() {
            tracing::trace!(
                target: "airc::addressed",
                event_id = %envelope.event_id,
                "addressed frame for another identity dropped unread"
            );
            return;
        }
        let from = envelope.sender;
        let wire = match envelope.body.as_ref().map(AddressedWire::from_body) {
            Some(Ok(wire)) => wire,
            Some(Err(error)) => return refuse(from, &error.to_string()),
            None => return refuse(from, "addressed frame has no body"),
        };
        let state = &self.inner.addressed;
        match wire {
            AddressedWire::Init(init) => {
                if init.peer_id != from {
                    return refuse(from, "handshake init names a different initiator");
                }
                match state.sessions.on_init(&init) {
                    Ok(resp) => {
                        state.session_installed();
                        if let Err(error) = self
                            .send_addressed_wire(from, &AddressedWire::Resp(resp))
                            .await
                        {
                            refuse(from, &format!("handshake response not sent: {error}"));
                        }
                    }
                    // Ours to them completes instead; nothing to answer.
                    Err(SessionManagerError::Crossed) => {}
                    Err(error) => refuse(from, &error.to_string()),
                }
            }
            AddressedWire::Resp(resp) => {
                if resp.peer_id != from {
                    return refuse(from, "handshake response names a different responder");
                }
                match state.sessions.on_resp(&resp) {
                    Ok(()) => state.session_installed(),
                    Err(error) => refuse(from, &error.to_string()),
                }
            }
            AddressedWire::Sealed(sealed) => {
                match state.sessions.open_from(from, &aad(from, me), &sealed) {
                    Ok(plaintext) => match SealedInner::from_plaintext(&plaintext) {
                        Ok(inner) => {
                            // No receiver is not a loss to report: the
                            // sender's own deadline answers for it.
                            let _ = state.inbox_tx.send(Arc::new(VerifiedAddressed {
                                from,
                                to: me,
                                kind: inner.kind,
                                correlation: inner.correlation,
                                headers: inner.headers,
                                body: inner.body,
                            }));
                        }
                        Err(error) => refuse(from, &error.to_string()),
                    },
                    // This side lost its session (a restart): re-key, so the
                    // sender's retry opens. Its current frame is gone, which
                    // its deadline reports.
                    Err(SessionManagerError::NoSession) => {
                        if state.sessions.has_pending(from) {
                            return;
                        }
                        let now = match now_ms() {
                            Ok(now) => now,
                            Err(error) => return refuse(from, &error.to_string()),
                        };
                        let init = state.sessions.begin_handshake(from, now);
                        if let Err(error) = self
                            .send_addressed_wire(from, &AddressedWire::Init(init))
                            .await
                        {
                            refuse(from, &format!("re-key init not sent: {error}"));
                        }
                    }
                    Err(error) => refuse(from, &error.to_string()),
                }
            }
        }
    }
}

/// A refused addressed frame: loud, never echoed (no payload is logged).
fn refuse(from: airc_core::PeerId, reason: &str) {
    tracing::warn!(target: "airc::addressed", %from, reason, "addressed frame refused");
}
