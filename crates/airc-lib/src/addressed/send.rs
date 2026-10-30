//! The sending end: open a session if there is none, seal, send.

use airc_core::{EventId, MentionTarget};
use airc_protocol::addressed::{aad, addressed_channel, AddressedWire, SealedInner};
use airc_protocol::{Envelope, Frame, FrameKind, Signature};
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use tokio::time::Instant;

use super::{AddressedError, AddressedSend};
use crate::time::now_ms;
use crate::Airc;

/// Weak leases avoid retaining a retry worker or an entry per historical peer.
/// The per-peer lock is held by one waiting request, and its next-attempt time
/// survives ownership handoff to other live waiters after cancellation.
type HandshakeSchedule = tokio::sync::Mutex<Instant>;

#[derive(Default)]
pub(crate) struct HandshakeWaiters {
    peers: tokio::sync::Mutex<HashMap<airc_core::PeerId, Weak<HandshakeSchedule>>>,
}

impl HandshakeWaiters {
    async fn lease(&self, peer: airc_core::PeerId) -> Arc<HandshakeSchedule> {
        let mut peers = self.peers.lock().await;
        peers.retain(|_, lease| lease.strong_count() != 0);
        if let Some(lease) = peers.get(&peer).and_then(Weak::upgrade) {
            return lease;
        }
        let lease = Arc::new(tokio::sync::Mutex::new(Instant::now()));
        peers.insert(peer, Arc::downgrade(&lease));
        lease
    }
}

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

    /// One request owns retransmission for this peer, using the existing route
    /// retry cadence. Re-send the same pending init until a session arrives or
    /// this caller expires. Followers share ownership, not their deadlines.
    async fn await_session(
        &self,
        peer: airc_core::PeerId,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        let state = &self.inner.addressed;
        tokio::time::timeout_at(deadline, async {
            let lease = state.handshake_waiters.lease(peer).await;
            let mut next_attempt = lease.lock().await;
            let mut established = state.established.subscribe();
            let retry_delay =
                crate::route_forwarder::RoutedForwarderConfig::default().retry_backoff;
            while !state.sessions.has_session(peer) {
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err(AddressedError::DeadlineExceeded);
                    }
                    changed = established.changed() => {
                        if changed.is_err() {
                            return Err(AddressedError::DeadlineExceeded);
                        }
                    }
                    _ = tokio::time::sleep_until(*next_attempt) => {
                        // Set before awaiting the write: cancellation must not
                        // let a live follower immediately repeat the attempt.
                        *next_attempt = Instant::now() + retry_delay;
                        self.rekey(peer, deadline).await?;
                        *next_attempt = Instant::now() + retry_delay;
                    }
                }
            }
            Ok(())
        })
        .await
        .unwrap_or(Err(AddressedError::DeadlineExceeded))
    }

    /// Put one addressed frame on the wire to `to`. Never appended anywhere.
    pub(super) async fn send_addressed_wire(
        &self,
        to: airc_core::PeerId,
        wire: &AddressedWire,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        if Instant::now() >= deadline {
            return Err(AddressedError::DeadlineExceeded);
        }
        tokio::time::timeout_at(deadline, self.write_addressed_wire(to, wire, deadline))
            .await
            .unwrap_or(Err(AddressedError::DeadlineExceeded))
    }

    async fn write_addressed_wire(
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
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(AddressedError::DeadlineExceeded)?;
        airc_protocol::addressed_deadline::stamp(
            &mut frame.envelope.headers,
            std::time::SystemTime::now(),
            remaining,
        )
        .map_err(|error| AddressedError::Codec(format!("addressed expiry: {error:?}")))?;
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

#[cfg(test)]
#[path = "send_retry_tests.rs"]
mod retry_tests;
