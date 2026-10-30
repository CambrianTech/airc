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
        if !matches!(&frame.envelope.signature, airc_protocol::Signature::Ed25519 { signer, .. } if *signer == frame.envelope.sender)
        {
            return refuse(frame.envelope.sender, "addressed original sender must sign");
        }
        if let Err(error) = airc_protocol::verify(
            &frame,
            airc_protocol::VerificationPolicy::Strict,
            &self.inner.registry,
        ) {
            return refuse(
                frame.envelope.sender,
                &format!("addressed sender signature: {error}"),
            );
        }
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
        let monotonic = tokio::time::Instant::now();
        let budget = match airc_protocol::addressed_deadline::remaining(
            &envelope.headers,
            std::time::SystemTime::now(),
        ) {
            Ok(budget) => budget.min(airc_ipc::client::DEFAULT_RPC_TIMEOUT),
            Err(error) => return refuse(from, &format!("sender expiry: {error:?}")),
        };
        let deadline = monotonic + budget;

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
                            .send_addressed_wire(from, &AddressedWire::Resp(resp), deadline)
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
                        Ok(inner) => self
                            .deliver_opened(VerifiedAddressed::opened(from, me, inner, deadline)),
                        Err(error) => refuse(from, &error.to_string()),
                    },
                    // This side lost its session (a restart): re-key, so the
                    // sender's retry opens. Its current frame is gone, which
                    // its deadline reports.
                    Err(SessionManagerError::NoSession) => self.rekey_logged(from, deadline).await,
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
                        self.rekey_logged(from, deadline).await;
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
        if tokio::time::Instant::now() >= verified.deadline {
            return refuse(from, "payload expired before delivery");
        }
        if verified.kind() == super::AddressedKind::Command
            && verified
                .headers()
                .get(crate::backfill::HEADER_AIRC_BACKFILL)
                .is_some_and(|value| value == "request")
        {
            let owner = self.clone();
            tokio::spawn(async move {
                match tokio::time::timeout_at(verified.deadline, owner.serve_backfill(&verified))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => refuse(from, &format!("addressed backfill failed: {error}")),
                    Err(_) => refuse(from, "addressed backfill expired"),
                }
            });
            return;
        }
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
    pub(super) async fn rekey(
        &self,
        peer: airc_core::PeerId,
        deadline: tokio::time::Instant,
    ) -> Result<(), super::AddressedError> {
        let sessions = &self.inner.addressed.sessions;
        let now = now_ms().map_err(|error| super::AddressedError::Transport(error.to_string()))?;
        let init = sessions.ensure_handshake(peer, now);
        self.send_addressed_wire(peer, &AddressedWire::Init(init), deadline)
            .await
    }

    async fn rekey_logged(&self, peer: airc_core::PeerId, deadline: tokio::time::Instant) {
        if let Err(error) = self.rekey(peer, deadline).await {
            refuse(peer, &format!("re-key init not sent: {error}"));
        }
    }
}

/// A refused addressed frame: loud, never echoed (no payload is logged).
fn refuse(from: airc_core::PeerId, reason: &str) {
    tracing::warn!(target: "airc::addressed", %from, reason, "addressed frame refused");
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    use airc_core::{Body, Headers};
    use airc_protocol::{Envelope, FrameKind, Signature};

    #[tokio::test]
    async fn intermediary_cannot_extend_expiry_or_consume_original_replay_counter() {
        let ah = tempfile::tempdir().unwrap();
        let bh = tempfile::tempdir().unwrap();
        let ch = tempfile::tempdir().unwrap();
        let alice = Airc::open(ah.path()).await.unwrap();
        let bob = Airc::open(bh.path()).await.unwrap();
        let carol = Airc::open(ch.path()).await.unwrap();
        alice
            .add_peer(bob.peer_spec().parse().unwrap())
            .await
            .unwrap();
        bob.add_peer(alice.peer_spec().parse().unwrap())
            .await
            .unwrap();
        bob.add_peer(carol.peer_spec().parse().unwrap())
            .await
            .unwrap();
        let init = alice
            .inner
            .addressed
            .sessions
            .ensure_handshake(bob.peer_id(), now_ms().unwrap());
        let resp = bob.inner.addressed.sessions.on_init(&init).unwrap();
        alice.inner.addressed.sessions.on_resp(&resp).unwrap();
        let plaintext = SealedInner {
            kind: super::super::AddressedKind::Command,
            correlation: uuid::Uuid::new_v4(),
            headers: Headers::new(),
            body: Body::text("original"),
        }
        .to_plaintext()
        .unwrap();
        let sealed = alice
            .inner
            .addressed
            .sessions
            .seal_for(
                bob.peer_id(),
                &aad(alice.peer_id(), bob.peer_id()),
                &plaintext,
            )
            .unwrap();
        let mut original = Frame {
            kind: FrameKind::Addressed,
            envelope: Envelope {
                event_id: airc_core::EventId::new(),
                sender: alice.peer_id(),
                sender_client: alice.inner.identity.client_id,
                channel: addressed_channel(),
                target: MentionTarget::Peer(bob.peer_id()),
                lamport: 1,
                occurred_at_ms: now_ms().unwrap(),
                reply_to: None,
                headers: Headers::new(),
                body: Some(AddressedWire::Sealed(sealed).to_body().unwrap()),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        };
        airc_protocol::addressed_deadline::stamp(
            &mut original.envelope.headers,
            std::time::SystemTime::now(),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        original.envelope.signature = alice
            .inner
            .identity
            .keypair
            .sign_envelope(&original.envelope, alice.peer_id(), 0)
            .unwrap();
        let mut forged = original.clone();
        airc_protocol::addressed_deadline::stamp(
            &mut forged.envelope.headers,
            std::time::SystemTime::now(),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
        forged.envelope.signature = carol
            .inner
            .identity
            .keypair
            .sign_envelope(&forged.envelope, carol.peer_id(), 0)
            .unwrap();
        let mut inbox = bob
            .addressed_inbox(tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await
            .unwrap();
        bob.receive_addressed(forged).await;
        assert!(matches!(
            inbox.rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        bob.receive_addressed(original).await;
        let accepted = tokio::time::timeout(std::time::Duration::from_secs(1), inbox.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(accepted.body().as_text(), Some("original"));
    }
}
