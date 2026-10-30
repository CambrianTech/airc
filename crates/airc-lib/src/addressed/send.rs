//! The sending end: open a session if there is none, seal, send.

use airc_core::{EventId, MentionTarget};
use airc_protocol::addressed::{aad, addressed_channel, AddressedWire, SealedInner};
use airc_protocol::{Envelope, Frame, FrameKind, Signature};
use tokio::time::Instant;

use super::{AddressedError, AddressedSend};
use crate::time::now_ms;
use crate::Airc;

impl Airc {
    /// Seal `msg` for `msg.to` and send it, all of it (session, seal, wire
    /// write) by `deadline`. Opens a session first when there is none. Nothing
    /// is stored here or by any peer that relays it.
    pub async fn send_addressed(
        &self,
        msg: AddressedSend,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        if Instant::now() >= deadline {
            return Err(AddressedError::DeadlineExceeded);
        }
        let _owner = self.addressed_daemon_owner(deadline).await?;
        tokio::time::timeout_at(deadline, self.send_addressed_by(msg, deadline))
            .await
            .unwrap_or(Err(AddressedError::DeadlineExceeded))
    }

    async fn send_addressed_by(
        &self,
        msg: AddressedSend,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        let state = &self.inner.addressed;
        if !state.sessions.has_session(msg.to) {
            self.await_session(msg.to, deadline).await?;
        }
        let me = self.peer_id();
        let plaintext = SealedInner {
            kind: msg.kind,
            correlation: msg.correlation,
            headers: msg.headers,
            body: msg.body,
        }
        .to_plaintext()
        .map_err(|error| AddressedError::Codec(error.to_string()))?;
        let sealed = state
            .sessions
            .seal_for(msg.to, &aad(me, msg.to), &plaintext)
            .map_err(AddressedError::Handshake)?;
        self.send_addressed_wire(msg.to, &AddressedWire::Sealed(sealed), deadline)
            .await
    }

    /// Send our init to `peer` (again, if one is unanswered), then wait for
    /// any session with it. Expiry ends only this caller's wait; other callers
    /// and a later retry retain the same outstanding authenticated init.
    async fn await_session(
        &self,
        peer: airc_core::PeerId,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        let state = &self.inner.addressed;
        let mut established = state.established.subscribe();
        self.rekey(peer, deadline).await?;
        let waited = tokio::time::timeout_at(deadline, async {
            while !state.sessions.has_session(peer) {
                if established.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
        if waited.is_err() || !state.sessions.has_session(peer) {
            return Err(AddressedError::DeadlineExceeded);
        }
        Ok(())
    }

    /// Put one addressed frame on the wire to `to`. Never appended anywhere.
    pub(super) async fn send_addressed_wire(
        &self,
        to: airc_core::PeerId,
        wire: &AddressedWire,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        let body = wire
            .to_body()
            .map_err(|error| AddressedError::Codec(error.to_string()))?;
        let occurred_at_ms =
            now_ms().map_err(|error| AddressedError::Transport(error.to_string()))?;
        let target = MentionTarget::Peer(to);
        let mut frame = Frame {
            kind: FrameKind::Addressed,
            envelope: Envelope {
                event_id: EventId::new(),
                sender: self.inner.identity.peer_id,
                sender_client: self.inner.identity.client_id,
                channel: addressed_channel(),
                target: target.clone(),
                lamport: self.next_lamport(occurred_at_ms),
                occurred_at_ms,
                reply_to: None,
                headers: Default::default(),
                body: Some(body),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        };
        frame.envelope.signature = self
            .inner
            .identity
            .keypair
            .sign_envelope(&frame.envelope, self.inner.identity.peer_id, 0)
            .map_err(|error| AddressedError::Codec(error.to_string()))?;
        if let Some(client) = &self.inner.daemon_client {
            return client
                .send_addressed(frame, deadline)
                .await
                .map_err(|error| AddressedError::Transport(error.to_string()));
        }
        let route = self
            .resolve_send_route(FrameKind::Addressed, &target)
            .await
            .map_err(|error| AddressedError::Transport(error.to_string()))?;
        self.execute_send_route(route.kind, frame)
            .await
            .map_err(|error| AddressedError::Transport(error.to_string()))
    }
}
