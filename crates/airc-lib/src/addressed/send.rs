//! The sending end: open a session if there is none, seal, send.

use airc_core::{EventId, MentionTarget};
use airc_protocol::addressed::{aad, addressed_channel, AddressedWire, SealedInner};
use airc_protocol::{Envelope, Frame, FrameKind, Signature};
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use tokio::time::Instant;

use super::{AddressedError, AddressedSend, SessionUnavailableReason};
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
    /// Seal `msg` for `msg.to` and send it, by `deadline`, in two phases with distinct errors.
    /// Pre-dispatch (owner attach, then a session if there is none) is bounded by the route's own
    /// delivery budget as well as `deadline`; any failure there is
    /// [`AddressedError::SessionUnavailable`] or a typed non-retryable cause, and nothing was
    /// sealed or sent. Then seal and write under `deadline`; a deadline there is
    /// [`AddressedError::DeadlineExceeded`], delivery unknown. Nothing is stored here or by any
    /// peer that relays it.
    pub async fn send_addressed(
        &self,
        msg: AddressedSend,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        let peer = msg.to;
        let (bound, bound_reason) = pre_dispatch_bound(Instant::now(), deadline);
        if Instant::now() >= deadline {
            return Err(AddressedError::SessionUnavailable {
                peer,
                reason: SessionUnavailableReason::CallerDeadline,
            });
        }
        let _owner = match self.addressed_daemon_owner(bound).await {
            Ok(owner) => owner,
            Err(AddressedError::DeadlineExceeded) => {
                return Err(AddressedError::SessionUnavailable {
                    peer,
                    reason: match bound_reason {
                        SessionUnavailableReason::CallerDeadline => bound_reason,
                        _ => SessionUnavailableReason::OwnerUnavailable,
                    },
                })
            }
            Err(other) => return Err(other),
        };
        if !self.inner.addressed.sessions.has_session(peer) {
            self.await_session(peer, bound, bound_reason).await?;
        }
        tokio::time::timeout_at(deadline, self.seal_and_send(msg, deadline))
            .await
            .unwrap_or(Err(AddressedError::DeadlineExceeded))
    }

    async fn seal_and_send(
        &self,
        msg: AddressedSend,
        deadline: Instant,
    ) -> Result<(), AddressedError> {
        let state = &self.inner.addressed;
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
    /// retry cadence. Re-send the same pending init until a session arrives or `bound`
    /// passes (the pre-dispatch bound, [`pre_dispatch_bound`]). Followers share ownership,
    /// not their deadlines. Nothing has been sealed or sent here, so giving up is
    /// [`AddressedError::SessionUnavailable`]; an init that could not go out is retried only
    /// when the route resolver said there is no live route, and every other failure
    /// (codec, signing, clock, IPC) returns at once, typed. Before this the wait was the
    /// caller's whole deadline (a remote inference request's ~630 s) and its failure read as
    /// a send deadline: IntelMac 2026-11-02, three residents waited that long per act on a
    /// seat that answered no handshake while the route delivered every other frame.
    async fn await_session(
        &self,
        peer: airc_core::PeerId,
        bound: Instant,
        bound_reason: SessionUnavailableReason,
    ) -> Result<(), AddressedError> {
        let state = &self.inner.addressed;
        let retry_backoff = crate::route_forwarder::RoutedForwarderConfig::default().retry_backoff;
        let mut last_init = InitOutcome::Sent;
        let waited = tokio::time::timeout_at(bound, async {
            let lease = state.handshake_waiters.lease(peer).await;
            let mut next_attempt = lease.lock().await;
            let mut established = state.established.subscribe();
            while !state.sessions.has_session(peer) {
                tokio::select! {
                    biased;
                    changed = established.changed() => {
                        if changed.is_err() {
                            return Err(AddressedError::Transport("addressed session state closed".into()));
                        }
                    }
                    _ = tokio::time::sleep_until(*next_attempt) => {
                        // Set before awaiting the write: cancellation must not
                        // let a live follower immediately repeat the attempt.
                        *next_attempt = Instant::now() + retry_backoff;
                        last_init = InitOutcome::of(self.rekey(peer, bound).await)?;
                        *next_attempt = Instant::now() + retry_backoff;
                    }
                }
            }
            Ok(())
        })
        .await;
        match waited {
            Ok(result) => result,
            Err(_) => Err(AddressedError::SessionUnavailable {
                peer,
                reason: last_init.give_up_reason(bound_reason),
            }),
        }
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
            .map_err(|error| match error {
                crate::error::AircError::Route(why) => AddressedError::NoLiveRoute(why),
                other => AddressedError::Transport(other.to_string()),
            })?;
        self.execute_send_route(route.kind, frame)
            .await
            .map_err(|error| AddressedError::Transport(error.to_string()))
    }
}

/// The pre-dispatch bound of one send: the route's own delivery budget
/// ([`crate::route_forwarder::RoutedForwarderConfig::delivery_budget`], the one owner of "how
/// long we try a peer") or the caller's deadline, whichever is first, and the reason a give-up
/// at that bound carries. Pure.
pub(super) fn pre_dispatch_bound(
    now: Instant,
    deadline: Instant,
) -> (Instant, SessionUnavailableReason) {
    let budget = now + crate::route_forwarder::RoutedForwarderConfig::default().delivery_budget();
    if budget < deadline {
        (budget, SessionUnavailableReason::NoHandshakeReply)
    } else {
        (deadline, SessionUnavailableReason::CallerDeadline)
    }
}

/// What one handshake init attempt came to: retried only for an absent route or a write the
/// bound cut short; every other failure is returned typed, never relabeled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitOutcome {
    Sent,
    NoRoute,
}

impl InitOutcome {
    pub(super) fn of(result: Result<(), AddressedError>) -> Result<Self, AddressedError> {
        match result {
            Ok(()) => Ok(Self::Sent),
            Err(AddressedError::NoLiveRoute(_)) => Ok(Self::NoRoute),
            // The bound passed mid-write: the session wait gives up on its own timer.
            Err(AddressedError::DeadlineExceeded) => Ok(Self::Sent),
            Err(other) => Err(other),
        }
    }

    /// The reason a give-up carries: the bound's own reason when the caller's deadline set it,
    /// else what the last init came to.
    pub(super) fn give_up_reason(
        self,
        bound_reason: SessionUnavailableReason,
    ) -> SessionUnavailableReason {
        match (bound_reason, self) {
            (SessionUnavailableReason::CallerDeadline, _) => {
                SessionUnavailableReason::CallerDeadline
            }
            (_, Self::NoRoute) => SessionUnavailableReason::NoRoute,
            (_, Self::Sent) => SessionUnavailableReason::NoHandshakeReply,
        }
    }
}

#[cfg(test)]
#[path = "send_retry_tests.rs"]
mod retry_tests;
