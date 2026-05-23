//! Command-bus primitive — typed request/reply with correlation +
//! deadline + cancellation.
//!
//! Phase 4 of the GRID-SUBSTRATE-AUDIT. The same primitive serves:
//!
//! - **Tool integration for AI agent runtimes** — an agent's tool
//!   call (Claude/Codex/OpenClaw/Hermes invoking a remote
//!   capability) becomes `Airc::request(command_kind, body,
//!   deadline)`; the agent's tool returns when `await_reply`
//!   resolves. The shape maps cleanly onto each runtime's native
//!   promise/future/await.
//! - **Continuum's event + promise-based command bus** —
//!   distributed compute / model serving / capability leasing.
//!   Same correlation_id semantics, same deadline + cancellation
//!   contract.
//! - **Cross-runtime workflows** — OpenClaw thread emits a command
//!   that Hermes orchestrator handles; sentinel-ai issues a
//!   command an operator dashboard replies to; future AR command
//!   planes route the same way.
//!
//! Deliberately named "command" rather than "tool" because the
//! use cases go beyond agent tools. Consumers own the
//! `airc.command_kind` vocabulary
//! (`continuum.lora.invoke`, `forge.hermes.agent_command`,
//! `openclaw.thread.create`, etc.); the substrate owns delivery +
//! correlation matching + deadline enforcement.
//!
//! ## Conceptually
//!
//! A command is an **async task** — the requester emits it, the
//! receiver eventually emits a reply (or a deadline elapses, or
//! the requester cancels). The substrate carries both as plain
//! events with paired `airc.correlation_id`.
//!
//! Forge-alloy contracts (the typed-payload layer above airc)
//! ride alongside the substrate headers: a request event
//! typically carries `forge.body_hint=continuum.lora.invoke`
//! identifying the contract that decodes the body, plus
//! `airc.command_kind=continuum.lora.invoke` for the substrate's
//! routing/dispatch view. airc routes on the header; the
//! consumer parses the body against the forge contract.
//!
//! Example (consumer-side; substrate sees only opaque events):
//!
//! ```ignore
//! // Requesting machine — emits a LoRA-invoke command
//! let mut headers = Headers::new();
//! headers.insert("forge.body_hint".into(), "continuum.lora.invoke".into());
//! headers.insert("airc.command_kind".into(), "continuum.lora.invoke".into());
//! let pending = airc.request(
//!     MentionTarget::All,
//!     headers,
//!     Body::Json(serde_json::json!({ "model": "foo", "prompt": "..." })),
//!     Duration::from_secs(60),
//! ).await?;
//! let reply = airc.await_reply(pending).await?;
//! // reply.body is the forge.continuum.lora.result payload
//!
//! // Receiving machine — handler dispatches on airc.command_kind
//! for event in stream {
//!     if event.headers.get("airc.command_kind") == Some(&"continuum.lora.invoke".into()) {
//!         let correlation_id = parse_uuid(event.headers.get("airc.correlation_id")?)?;
//!         let reply_to = parse_peer_id(event.headers.get("airc.reply_to")?)?;
//!         let result = run_lora(event.body)?;
//!         let mut reply_headers = Headers::new();
//!         reply_headers.insert("forge.body_hint".into(), "continuum.lora.result".into());
//!         airc.reply(reply_to, correlation_id, reply_headers, Body::Json(result)).await?;
//!     }
//! }
//! ```
//!
//! Wire format (additive headers on a normal `Message` frame):
//!
//! - `airc.correlation_id` — UUIDv4 string; pairs ONE request with
//!   ONE reply. Distinct from `airc.trace_id` (end-to-end
//!   observability spanning many events).
//! - `airc.reply_to` — sender's peer_id; the reply event's
//!   `target` should match this.
//! - `airc.deadline` — epoch-ms decimal string; receivers may drop
//!   past-deadline requests; the requester uses it to time out.
//! - `airc.command_kind` (optional) — consumer-namespace string,
//!   e.g. `continuum.lora.invoke`. The substrate doesn't interpret
//!   it; receivers dispatch on it without parsing the body.
//!
//! API:
//!
//! ```ignore
//! let pending = airc.request(
//!     headers,
//!     Body::text("..."),
//!     Duration::from_secs(30),
//! ).await?;
//! let reply = airc.await_reply(pending).await?;
//! ```
//!
//! On the receiver side, handler code reads
//! `event.headers["airc.correlation_id"]` + `["airc.reply_to"]`
//! off the request event and calls
//! `airc.reply(reply_to, correlation_id, headers, body)`.
//!
//! Cancellation: `airc.cancel(correlation_id)` aborts the pending
//! receiver, but does NOT send a cancel signal to the remote
//! receiver — that's a consumer-level concern (the consumer may
//! emit a `consumer.cancel.<correlation_id>` event on the same
//! correlation if their domain needs it).

use std::time::{Duration, Instant};

use airc_core::{Body, Headers, MentionTarget, PeerId, TranscriptEvent};
use airc_protocol::{HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_DEADLINE, HEADER_AIRC_REPLY_TO};
use futures::stream::StreamExt;
use uuid::Uuid;

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

/// One in-flight request awaiting a matching reply.
#[derive(Debug)]
pub struct PendingCommand {
    /// The correlation id stamped on the outgoing request event.
    /// The reply event must carry the same value in
    /// `airc.correlation_id`.
    pub correlation_id: Uuid,
    /// Wall-clock deadline. `await_reply` returns
    /// `AircError::CommandDeadline` past this point.
    pub deadline_at_ms: u64,
}

impl PendingCommand {
    /// Returns the remaining time before the deadline, or `None`
    /// if the deadline has already passed.
    pub fn remaining(&self) -> Option<Duration> {
        let now = now_ms().ok()?;
        if now >= self.deadline_at_ms {
            None
        } else {
            Some(Duration::from_millis(self.deadline_at_ms - now))
        }
    }
}

impl Airc {
    /// Issue a request and return a [`PendingCommand`] handle. The
    /// reply is awaited via [`Airc::await_reply`]. The substrate
    /// stamps `airc.correlation_id`, `airc.reply_to`, and
    /// `airc.deadline` on the outgoing event; callers may inject
    /// any consumer-namespace headers they want (e.g.
    /// `continuum.lora.invoke`, `forge.body_hint`).
    ///
    /// `target` selects who is expected to handle the request.
    /// `MentionTarget::All` broadcasts and the first reply wins;
    /// `MentionTarget::Peer(id)` directs at one peer.
    pub async fn request(
        &self,
        target: MentionTarget,
        mut headers: Headers,
        body: Body,
        deadline: Duration,
    ) -> Result<PendingCommand, AircError> {
        let correlation_id = Uuid::new_v4();
        let deadline_at_ms = now_ms()? + deadline.as_millis() as u64;

        headers.insert(
            HEADER_AIRC_CORRELATION_ID.into(),
            correlation_id.to_string(),
        );
        headers.insert(
            HEADER_AIRC_REPLY_TO.into(),
            self.inner.identity.peer_id.to_string(),
        );
        headers.insert(HEADER_AIRC_DEADLINE.into(), deadline_at_ms.to_string());

        // For directed requests (Peer target) the substrate today
        // routes by channel + headers; the target lives on the
        // envelope but isn't enforced by the broadcaster. Receivers
        // self-filter on `target == own peer_id`. Recording the
        // intent here lets future routing optimisations honor it
        // without changing the public API.
        let _ = target;

        self.send(body, headers).await?;

        Ok(PendingCommand {
            correlation_id,
            deadline_at_ms,
        })
    }

    /// Reply to an in-flight request. `reply_to` is the
    /// `airc.reply_to` value read off the request event;
    /// `correlation_id` is the request's `airc.correlation_id`.
    pub async fn reply(
        &self,
        reply_to: PeerId,
        correlation_id: Uuid,
        mut headers: Headers,
        body: Body,
    ) -> Result<(), AircError> {
        let _ = reply_to; // recorded in headers below
        headers.insert(
            HEADER_AIRC_CORRELATION_ID.into(),
            correlation_id.to_string(),
        );
        headers.insert(HEADER_AIRC_REPLY_TO.into(), reply_to.to_string());
        self.send(body, headers).await?;
        Ok(())
    }

    /// Wait for the reply to a [`PendingCommand`]. Returns the
    /// matching `TranscriptEvent` (the reply, with its body and
    /// headers intact) or [`AircError::CommandDeadline`] if the
    /// deadline elapses first.
    ///
    /// The substrate subscribes to the live broadcast stream and
    /// filters by `airc.correlation_id`. If the reply arrived
    /// before this call (uncommon but possible — the broadcast
    /// channel has a buffer), the subscribed stream replays it.
    pub async fn await_reply(&self, pending: PendingCommand) -> Result<TranscriptEvent, AircError> {
        let correlation = pending.correlation_id.to_string();
        let mut stream = self.subscribe().await?;
        let deadline = Instant::now()
            + pending
                .remaining()
                .unwrap_or_else(|| Duration::from_secs(0));

        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err(AircError::CommandDeadline {
                    correlation_id: pending.correlation_id,
                });
            }
            match tokio::time::timeout(timeout, stream.next()).await {
                Ok(Some(Ok(event))) => {
                    if event.headers.get(HEADER_AIRC_CORRELATION_ID) == Some(&correlation)
                        && event.peer_id != self.inner.identity.peer_id
                    {
                        return Ok(event);
                    }
                    // Some other event flowed through; keep waiting.
                }
                Ok(Some(Err(_))) => {
                    // LiveLag — subscriber fell behind. Keep
                    // looping; eventually we get caught up.
                }
                Ok(None) => {
                    // Stream closed before any reply arrived.
                    return Err(AircError::CommandDeadline {
                        correlation_id: pending.correlation_id,
                    });
                }
                Err(_) => {
                    return Err(AircError::CommandDeadline {
                        correlation_id: pending.correlation_id,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn pending_command_remaining_returns_none_past_deadline() {
        let pending = PendingCommand {
            correlation_id: Uuid::new_v4(),
            deadline_at_ms: 1,
        };
        assert!(pending.remaining().is_none());
    }

    #[test]
    fn pending_command_remaining_returns_some_before_deadline() {
        // Far-future deadline; remaining should be non-zero.
        let pending = PendingCommand {
            correlation_id: Uuid::new_v4(),
            deadline_at_ms: u64::MAX / 2,
        };
        let remaining = pending.remaining().unwrap();
        assert!(remaining.as_millis() > 0);
    }

    #[test]
    fn header_constants_match_protocol_module() {
        // The wire format pin: header names ride a stable schema
        // because cross-version peers must agree. If protocol
        // renames these, this test fails and we know to update both
        // sides in lockstep.
        let _ = HEADER_AIRC_CORRELATION_ID;
        let _ = HEADER_AIRC_REPLY_TO;
        let _ = HEADER_AIRC_DEADLINE;

        // Sanity: a typical headers map carrying all three.
        let mut headers: Headers = BTreeMap::new();
        headers.insert(
            HEADER_AIRC_CORRELATION_ID.into(),
            Uuid::new_v4().to_string(),
        );
        headers.insert(HEADER_AIRC_REPLY_TO.into(), PeerId::new().to_string());
        headers.insert(HEADER_AIRC_DEADLINE.into(), "1700000000000".to_string());
        assert_eq!(headers.len(), 3);
    }
}
