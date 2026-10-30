//! The receiving end: every inbound `FrameKind::Addressed` frame comes here
//! before anything could store it, and nothing here stores it.

use std::sync::Arc;

use airc_core::MentionTarget;
use airc_protocol::addressed::{aad, addressed_channel, AddressedWire, SealedInner};
use airc_protocol::{Frame, SessionError, SessionManagerError};

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
                            self.deliver_opened(VerifiedAddressed::opened(from, me, inner))
                        }
                        Err(error) => refuse(from, &error.to_string()),
                    },
                    // This side lost its session (a restart): re-key, so the
                    // sender's retry opens. Its current frame is gone, which
                    // its deadline reports.
                    Err(SessionManagerError::NoSession) => self.rekey_logged(from).await,
                    // The same frame by a second route: already delivered.
                    Err(SessionManagerError::Session(SessionError::Replay)) => {}
                    // The peer seals under a session this side does not hold
                    // (ours was replaced, e.g. by a replayed init after a
                    // restart): re-key, one init per unopenable frame.
                    Err(
                        error @ SessionManagerError::Session(
                            SessionError::AeadFailed | SessionError::CounterExhausted,
                        ),
                    ) => {
                        refuse(from, &error.to_string());
                        self.rekey_logged(from).await;
                    }
                    Err(error) => refuse(from, &error.to_string()),
                }
            }
        }
    }
}

impl Airc {
    /// The one place an opened payload goes. Delivery is at most once per
    /// frame (the session's replay window); a sender's retry is a new frame
    /// with its original correlation, so executing a command once is the
    /// consumer's dedup on (from, correlation).
    fn deliver_opened(&self, verified: VerifiedAddressed) {
        let from = verified.from();
        // An opened payload with no consumer is lost: loud, never a quiet
        // delivery.
        if self
            .inner
            .addressed
            .inbox_tx
            .send(Arc::new(verified))
            .is_err()
        {
            refuse(from, "opened, but no addressed inbox is consuming");
        }
    }

    /// Open, or re-open, the session with `peer`: send our unanswered init
    /// again (the responder answers a duplicate with the reply it already
    /// gave), or start one. Called once per frame that could not be opened
    /// and once per send attempt, so it heals a lost init or reply without a
    /// timer and never storms.
    pub(super) async fn rekey(&self, peer: airc_core::PeerId) -> Result<(), super::AddressedError> {
        let sessions = &self.inner.addressed.sessions;
        let init = match sessions.pending_init(peer) {
            Some(init) => init,
            None => {
                let now = now_ms()
                    .map_err(|error| super::AddressedError::Transport(error.to_string()))?;
                sessions.begin_handshake(peer, now)
            }
        };
        self.send_addressed_wire(peer, &AddressedWire::Init(init))
            .await
    }

    async fn rekey_logged(&self, peer: airc_core::PeerId) {
        if let Err(error) = self.rekey(peer).await {
            refuse(peer, &format!("re-key init not sent: {error}"));
        }
    }
}

/// A refused addressed frame: loud, never echoed (no payload is logged).
fn refuse(from: airc_core::PeerId, reason: &str) {
    tracing::warn!(target: "airc::addressed", %from, reason, "addressed frame refused");
}
