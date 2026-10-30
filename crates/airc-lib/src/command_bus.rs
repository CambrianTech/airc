//! Addressed command exchange over authenticated, sealed identity channels.
//!
//! Requests and results never enter a conversational room. The inbox is armed
//! before sending, and the reply must match the authenticated peer, recipient,
//! kind and correlation. Consumers decode bodies only after that boundary.

use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use airc_core::{Body, Headers, MentionTarget, PeerId};
use airc_protocol::{HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_DEADLINE, HEADER_AIRC_REPLY_TO};
use uuid::Uuid;

use crate::error::AircError;
use crate::time::now_ms;
use crate::{AddressedInbox, AddressedKind, AddressedSend, Airc, VerifiedAddressed};

/// Authenticated progress cannot settle a command. Only Completed is its result.
#[derive(Debug)]
pub enum CommandEvent {
    Progress(Arc<VerifiedAddressed>),
    Completed(Arc<VerifiedAddressed>),
}

/// An armed reply receiver with one monotonic end-to-end deadline.
pub struct PendingCommand {
    pub correlation_id: Uuid,
    pub deadline_at_ms: u64,
    deadline: Instant,
    inbox: Option<AddressedInbox>,
    expected_peer: PeerId,
    requester: PeerId,
}

impl std::fmt::Debug for PendingCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingCommand")
            .field("correlation_id", &self.correlation_id)
            .field("deadline_at_ms", &self.deadline_at_ms)
            .finish_non_exhaustive()
    }
}

impl PendingCommand {
    #[cfg(test)]
    pub(crate) fn for_test(
        inbox: AddressedInbox,
        expected_peer: PeerId,
        requester: PeerId,
        correlation_id: Uuid,
        deadline: Instant,
    ) -> Self {
        Self {
            inbox: Some(inbox),
            expected_peer,
            requester,
            correlation_id,
            deadline,
            deadline_at_ms: 0,
        }
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.checked_duration_since(Instant::now())
    }
}

/// Only authenticated command requests have a reply address. A caller-supplied
/// reply_to header never overrides the session-authenticated sender.
pub fn reply_addressing(event: &VerifiedAddressed) -> Option<(PeerId, Uuid)> {
    (event.kind() == AddressedKind::Command).then(|| (event.from(), event.correlation()))
}

fn direct_peer(target: MentionTarget) -> Result<PeerId, AircError> {
    match target {
        MentionTarget::Peer(peer) => Ok(peer),
        MentionTarget::All | MentionTarget::Room(_) => Err(AircError::Transport(
            "commands require an explicit peer recipient".into(),
        )),
    }
}

fn matches_reply(
    kind: AddressedKind,
    actual: (PeerId, PeerId, Uuid),
    expected: (PeerId, PeerId, Uuid),
) -> bool {
    matches!(
        kind,
        AddressedKind::CommandResult | AddressedKind::CommandProgress
    ) && actual == expected
}

impl Airc {
    /// Issue a sealed request to one peer. Broadcast and room targets refuse.
    /// The deadline covers handshake, delivery and waiting for the response.
    pub async fn request(
        &self,
        target: MentionTarget,
        mut headers: Headers,
        body: Body,
        timeout: Duration,
    ) -> Result<PendingCommand, AircError> {
        let peer = direct_peer(target)?;
        let correlation_id = Uuid::new_v4();
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            AircError::Transport("command duration exceeds monotonic clock range".into())
        })?;
        let millis = u64::try_from(timeout.as_millis())
            .map_err(|_| AircError::Transport("command duration exceeds wire range".into()))?;
        let deadline_at_ms = now_ms()?
            .checked_add(millis)
            .ok_or_else(|| AircError::Transport("command deadline exceeds wire range".into()))?;
        let inbox = self
            .addressed_inbox(deadline)
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        headers.insert(
            HEADER_AIRC_CORRELATION_ID.into(),
            correlation_id.to_string(),
        );
        headers.insert(HEADER_AIRC_REPLY_TO.into(), self.peer_id().to_string());
        headers.insert(HEADER_AIRC_DEADLINE.into(), deadline_at_ms.to_string());
        self.send_addressed(
            AddressedSend {
                to: peer,
                kind: AddressedKind::Command,
                correlation: correlation_id,
                headers,
                body,
            },
            deadline,
        )
        .await
        .map_err(|error| AircError::Transport(error.to_string()))?;
        Ok(PendingCommand {
            correlation_id,
            deadline_at_ms,
            deadline,
            inbox: Some(inbox),
            expected_peer: peer,
            requester: self.peer_id(),
        })
    }

    /// Reply to an authenticated request within its original deadline.
    pub async fn reply(
        &self,
        request: &VerifiedAddressed,
        headers: Headers,
        body: Body,
    ) -> Result<(), AircError> {
        self.respond(request, headers, body, AddressedKind::CommandResult)
            .await
    }

    /// Send progress inside the original exchange; even its final chunk is not
    /// the settled command result.
    pub async fn reply_progress(
        &self,
        request: &VerifiedAddressed,
        headers: Headers,
        body: Body,
    ) -> Result<(), AircError> {
        self.respond(request, headers, body, AddressedKind::CommandProgress)
            .await
    }

    async fn respond(
        &self,
        request: &VerifiedAddressed,
        mut headers: Headers,
        body: Body,
        kind: AddressedKind,
    ) -> Result<(), AircError> {
        if request.kind() != AddressedKind::Command || request.to() != self.peer_id() {
            return Err(AircError::Transport(
                "reply requires a verified request addressed to this identity".into(),
            ));
        }
        let deadline_at_ms = request
            .headers()
            .get(HEADER_AIRC_DEADLINE)
            .ok_or_else(|| AircError::Transport("request has no deadline".into()))?
            .parse::<u64>()
            .map_err(|_| AircError::Transport("request deadline is invalid".into()))?;
        let remaining = deadline_at_ms
            .checked_sub(now_ms()?)
            .filter(|ms| *ms > 0)
            .ok_or(AircError::CommandDeadline {
                correlation_id: request.correlation(),
            })?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(remaining))
            .ok_or_else(|| {
                AircError::Transport("reply deadline exceeds monotonic clock range".into())
            })?
            .min(request.deadline());
        if Instant::now() >= deadline {
            return Err(AircError::CommandDeadline {
                correlation_id: request.correlation(),
            });
        }
        headers.insert(
            HEADER_AIRC_CORRELATION_ID.into(),
            request.correlation().to_string(),
        );
        headers.insert(HEADER_AIRC_REPLY_TO.into(), request.from().to_string());
        self.send_addressed(
            AddressedSend {
                to: request.from(),
                kind,
                correlation: request.correlation(),
                headers,
                body,
            },
            deadline,
        )
        .await
        .map_err(|error| AircError::Transport(error.to_string()))
    }

    /// Return only the result authenticated for this exact exchange.
    pub async fn await_reply(
        &self,
        mut pending: PendingCommand,
    ) -> Result<Arc<VerifiedAddressed>, AircError> {
        loop {
            if let CommandEvent::Completed(result) = self.next_command_event(&mut pending).await? {
                return Ok(result);
            }
        }
    }

    /// Read the next authenticated event from the one inbox armed before send.
    /// Completion or a terminal error releases its lease; subsequent reads fail
    /// with CommandFinished. Progress bodies are application-defined.
    pub async fn next_command_event(
        &self,
        pending: &mut PendingCommand,
    ) -> Result<CommandEvent, AircError> {
        let result = self.read_command_event(pending).await;
        if !matches!(&result, Ok(CommandEvent::Progress(_))) {
            pending.inbox.take();
        }
        result
    }

    async fn read_command_event(
        &self,
        pending: &mut PendingCommand,
    ) -> Result<CommandEvent, AircError> {
        loop {
            if pending.inbox.is_none() {
                return Err(AircError::CommandFinished {
                    correlation_id: pending.correlation_id,
                });
            }
            let remaining = pending
                .remaining()
                .filter(|duration| !duration.is_zero())
                .ok_or(AircError::CommandDeadline {
                    correlation_id: pending.correlation_id,
                })?;
            let inbox = pending.inbox.as_mut().ok_or(AircError::CommandFinished {
                correlation_id: pending.correlation_id,
            })?;
            let event = tokio::time::timeout(remaining, inbox.next())
                .await
                .map_err(|_| AircError::CommandDeadline {
                    correlation_id: pending.correlation_id,
                })?;
            let event = match event {
                Ok(event) => event,
                Err(crate::addressed::InboxError::Expired {
                    from,
                    to,
                    kind,
                    correlation,
                }) => {
                    if matches_reply(
                        kind,
                        (from, to, correlation),
                        (
                            pending.expected_peer,
                            pending.requester,
                            pending.correlation_id,
                        ),
                    ) {
                        return Err(AircError::CommandDeadline {
                            correlation_id: pending.correlation_id,
                        });
                    }
                    continue;
                }
                Err(error) => {
                    return Err(AircError::CommandInbox {
                        correlation_id: pending.correlation_id,
                        source: error,
                    })
                }
            };
            if matches_reply(
                event.kind(),
                (event.from(), event.to(), event.correlation()),
                (
                    pending.expected_peer,
                    pending.requester,
                    pending.correlation_id,
                ),
            ) {
                match event.kind() {
                    AddressedKind::CommandProgress => return Ok(CommandEvent::Progress(event)),
                    AddressedKind::CommandResult => return Ok(CommandEvent::Completed(event)),
                    AddressedKind::Command => continue,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_requires_exact_authenticated_exchange() {
        let expected = (PeerId::new(), PeerId::new(), Uuid::new_v4());
        assert!(matches_reply(
            AddressedKind::CommandResult,
            expected,
            expected
        ));
        assert!(!matches_reply(AddressedKind::Command, expected, expected));
        for wrong in [
            (PeerId::new(), expected.1, expected.2),
            (expected.0, PeerId::new(), expected.2),
            (expected.0, expected.1, Uuid::new_v4()),
            (expected.1, expected.0, expected.2),
        ] {
            assert!(!matches_reply(
                AddressedKind::CommandResult,
                wrong,
                expected
            ));
        }
    }

    #[test]
    fn command_recipient_requires_explicit_peer() {
        let peer = PeerId::new();
        assert_eq!(direct_peer(MentionTarget::Peer(peer)).unwrap(), peer);
        assert!(direct_peer(MentionTarget::All).is_err());
        assert!(direct_peer(MentionTarget::Room(airc_core::RoomId::new())).is_err());
    }
}
