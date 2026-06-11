//! Continuum integration shape: persona/activity events.
//!
//! A Continuum persona runs as an AIRC peer. Activities scope a span
//! of persona work (an "activity" can be a chat session, a render
//! job, a multi-turn reasoning task). Events ride AIRC envelopes
//! with typed bodies + filterable headers so other Continuum
//! components (record/replay, RAG, manager-hat) can subscribe by
//! activity or by persona without parsing the body.
//!
//! Wire-level body hint: `forge.persona.event.v1`.
//!
//! Header conventions:
//!   - `forge.persona.kind`  — event variant discriminator (cheap filter)
//!   - `forge.persona.id`    — the persona that emitted the event
//!   - `forge.continuum.activity_id` — scoping activity, when applicable
//!   - `forge.continuum.turn_id`     — per-turn id, when applicable
//!   - `forge.persona.model_hint`    — optional model preference on a
//!     `TurnRequested`, when set
//!
//! Command-bus carriage (card ee2a339f): `TurnRequested` /
//! `TurnEmitted` double as the request/reply pair of
//! [`airc_lib::command_bus`]. The substrate stamps its own
//! `airc.correlation_id` / `airc.reply_to` / `airc.deadline` headers
//! on the request (via [`Airc::request`]); the persona-side responder
//! reads them back through [`turn_reply_address`] and echoes the
//! correlation via [`reply_turn_emitted`] so the requester's
//! [`Airc::await_reply`] resolves with the `TurnEmitted` event.

use airc_lib::{
    Airc, AircError, Body, EventFilter, HeaderFilter, Headers, MentionTarget, PeerId,
    PendingCommand, HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_DEADLINE, HEADER_AIRC_REPLY_TO,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::time::Duration;
use uuid::Uuid;

pub const BODY_HINT_FORGE_PERSONA_EVENT: &str = "forge.persona.event.v1";

pub const HEADER_FORGE_PERSONA_KIND: &str = "forge.persona.kind";
pub const HEADER_FORGE_PERSONA_ID: &str = "forge.persona.id";
pub const HEADER_FORGE_CONTINUUM_ACTIVITY_ID: &str = "forge.continuum.activity_id";
pub const HEADER_FORGE_CONTINUUM_TURN_ID: &str = "forge.continuum.turn_id";
/// Optional model preference projected off a [`TurnRequested`] so
/// schedulers can route a turn to a capable persona host without
/// decoding the body. Consumer-owned `forge.persona.*` namespace.
pub const HEADER_FORGE_PERSONA_MODEL_HINT: &str = "forge.persona.model_hint";

const HEADER_FORGE_BODY_HINT: &str = "forge.body_hint";

/// Persona/activity events. Representative subset — real integrations
/// extend with their own variants following the same shape (typed
/// noun struct + variant in this enum + header projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersonaEvent {
    /// A persona is requested to take a turn within an activity.
    TurnRequested(TurnRequested),
    /// A persona produced output for a turn it was given.
    TurnEmitted(TurnEmitted),
    /// A long-running activity started — other consumers may attach
    /// subscribers scoped to its `activity_id`.
    ActivityStarted(ActivityStarted),
    /// An activity concluded; bound subscriptions can detach.
    ActivityEnded(ActivityEnded),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRequested {
    pub persona_id: String,
    pub activity_id: String,
    pub turn_id: String,
    pub prompt: String,
    /// Optional model preference (adapter / checkpoint label) the
    /// requester wants the persona runtime to honor. Additive +
    /// optional so pre-existing encoded bodies still decode.
    /// Projected to `forge.persona.model_hint` when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<String>,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEmitted {
    pub persona_id: String,
    pub activity_id: String,
    pub turn_id: String,
    /// Persona-produced output. Real integrations may attach media
    /// refs separately on the envelope; this body field is the
    /// text portion.
    pub text: String,
    pub emitted_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityStarted {
    pub persona_id: String,
    pub activity_id: String,
    pub label: String,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEnded {
    pub persona_id: String,
    pub activity_id: String,
    pub ended_at_ms: u64,
}

impl PersonaEvent {
    pub fn persona_id(&self) -> &str {
        match self {
            Self::TurnRequested(e) => &e.persona_id,
            Self::TurnEmitted(e) => &e.persona_id,
            Self::ActivityStarted(e) => &e.persona_id,
            Self::ActivityEnded(e) => &e.persona_id,
        }
    }

    pub fn activity_id(&self) -> &str {
        match self {
            Self::TurnRequested(e) => &e.activity_id,
            Self::TurnEmitted(e) => &e.activity_id,
            Self::ActivityStarted(e) => &e.activity_id,
            Self::ActivityEnded(e) => &e.activity_id,
        }
    }

    pub fn turn_id(&self) -> Option<&str> {
        match self {
            Self::TurnRequested(e) => Some(&e.turn_id),
            Self::TurnEmitted(e) => Some(&e.turn_id),
            Self::ActivityStarted(_) | Self::ActivityEnded(_) => None,
        }
    }

    pub fn model_hint(&self) -> Option<&str> {
        match self {
            Self::TurnRequested(e) => e.model_hint.as_deref(),
            Self::TurnEmitted(_) | Self::ActivityStarted(_) | Self::ActivityEnded(_) => None,
        }
    }

    fn variant_kind(&self) -> &'static str {
        match self {
            Self::TurnRequested(_) => "turn_requested",
            Self::TurnEmitted(_) => "turn_emitted",
            Self::ActivityStarted(_) => "activity_started",
            Self::ActivityEnded(_) => "activity_ended",
        }
    }
}

#[derive(Debug)]
pub enum PersonaCodecError {
    MissingBody,
    NonJsonBody,
    BodyHintMismatch {
        actual: Option<String>,
        expected: &'static str,
    },
    /// A command-bus header required for the turn request/reply pairing
    /// is absent (the request did not travel via [`Airc::request`]).
    MissingHeader {
        header: &'static str,
    },
    /// A command-bus header is present but does not parse (correlation /
    /// reply-to must be UUIDs; deadline must be decimal epoch-ms).
    MalformedHeader {
        header: &'static str,
        value: String,
    },
    Json(serde_json::Error),
}

impl std::fmt::Display for PersonaCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBody => f.write_str("persona event body missing"),
            Self::NonJsonBody => f.write_str("persona event body must be JSON"),
            Self::BodyHintMismatch { actual, expected } => write!(
                f,
                "persona event body hint mismatch: actual={actual:?}, expected={expected:?}",
            ),
            Self::MissingHeader { header } => {
                write!(f, "persona turn request missing required header {header:?}")
            }
            Self::MalformedHeader { header, value } => {
                write!(
                    f,
                    "persona turn request header {header:?} malformed: {value:?}"
                )
            }
            Self::Json(error) => write!(f, "persona event JSON codec failed: {error}"),
        }
    }
}

impl std::error::Error for PersonaCodecError {}

impl From<serde_json::Error> for PersonaCodecError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Produce `(Headers, Body)` ready to feed into [`airc_lib::Airc::send`].
/// Domain-specific headers are projected so subscribers can filter
/// without decoding the body.
pub fn encode_persona_event(event: &PersonaEvent) -> Result<(Headers, Body), PersonaCodecError> {
    let mut headers = Headers::new();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_PERSONA_EVENT.to_string(),
    );
    headers.insert(
        HEADER_FORGE_PERSONA_KIND.to_string(),
        event.variant_kind().to_string(),
    );
    headers.insert(
        HEADER_FORGE_PERSONA_ID.to_string(),
        event.persona_id().to_string(),
    );
    headers.insert(
        HEADER_FORGE_CONTINUUM_ACTIVITY_ID.to_string(),
        event.activity_id().to_string(),
    );
    if let Some(turn_id) = event.turn_id() {
        headers.insert(
            HEADER_FORGE_CONTINUUM_TURN_ID.to_string(),
            turn_id.to_string(),
        );
    }
    if let Some(model_hint) = event.model_hint() {
        headers.insert(
            HEADER_FORGE_PERSONA_MODEL_HINT.to_string(),
            model_hint.to_string(),
        );
    }
    let body = Body::Json(serde_json::to_value(event)?);
    Ok((headers, body))
}

pub fn decode_persona_event(
    headers: &Headers,
    body: Option<&Body>,
) -> Result<PersonaEvent, PersonaCodecError> {
    match headers.get(HEADER_FORGE_BODY_HINT) {
        Some(value) if value == BODY_HINT_FORGE_PERSONA_EVENT => {}
        actual => {
            return Err(PersonaCodecError::BodyHintMismatch {
                actual: actual.cloned(),
                expected: BODY_HINT_FORGE_PERSONA_EVENT,
            });
        }
    }
    let body = body.ok_or(PersonaCodecError::MissingBody)?;
    let Body::Json(value) = body else {
        return Err(PersonaCodecError::NonJsonBody);
    };
    Ok(serde_json::from_value(value.clone())?)
}

// ---------------------------------------------------------------------------
// Command-bus carriage — card ee2a339f (persona-peer 3/8)
// ---------------------------------------------------------------------------
//
// The header names below are NOT a parallel vocabulary: they are the
// exact `airc.correlation_id` / `airc.reply_to` / `airc.deadline`
// constants the command bus itself stamps (re-exported by airc-lib
// from airc-protocol's `headers_keys`). The persona contract only
// reads them back; generic correlation plumbing stays in
// `airc_lib::command_bus`.

/// Command-bus addressing read off a [`TurnRequested`] request event.
///
/// [`Airc::request`] stamps `airc.correlation_id`, `airc.reply_to`,
/// and `airc.deadline` on the outgoing event; the persona-side
/// responder recovers them through this typed view to reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnReplyAddress {
    /// Pairs the reply with the in-flight request
    /// (`airc.correlation_id`). [`Airc::await_reply`] resolves only
    /// when the reply echoes this exact value.
    pub correlation_id: Uuid,
    /// Requester peer the reply is directed at (`airc.reply_to`).
    pub reply_to: PeerId,
    /// Requester-stamped wall-clock deadline (`airc.deadline`,
    /// epoch-ms). Responders may drop turns whose deadline already
    /// passed instead of doing dead work.
    pub deadline_at_ms: Option<u64>,
}

/// Read the command-bus reply address off a request event's headers.
/// Errors loudly when the correlation/reply-to pairing is absent or
/// malformed — a turn request that cannot be replied to is a bug, not
/// a silently droppable event.
pub fn turn_reply_address(headers: &Headers) -> Result<TurnReplyAddress, PersonaCodecError> {
    let correlation =
        headers
            .get(HEADER_AIRC_CORRELATION_ID)
            .ok_or(PersonaCodecError::MissingHeader {
                header: HEADER_AIRC_CORRELATION_ID,
            })?;
    let correlation_id =
        Uuid::parse_str(correlation).map_err(|_| PersonaCodecError::MalformedHeader {
            header: HEADER_AIRC_CORRELATION_ID,
            value: correlation.clone(),
        })?;
    let reply_to_raw =
        headers
            .get(HEADER_AIRC_REPLY_TO)
            .ok_or(PersonaCodecError::MissingHeader {
                header: HEADER_AIRC_REPLY_TO,
            })?;
    let reply_to = Uuid::parse_str(reply_to_raw)
        .map(PeerId::from_uuid)
        .map_err(|_| PersonaCodecError::MalformedHeader {
            header: HEADER_AIRC_REPLY_TO,
            value: reply_to_raw.clone(),
        })?;
    let deadline_at_ms = match headers.get(HEADER_AIRC_DEADLINE) {
        Some(raw) => Some(
            raw.parse::<u64>()
                .map_err(|_| PersonaCodecError::MalformedHeader {
                    header: HEADER_AIRC_DEADLINE,
                    value: raw.clone(),
                })?,
        ),
        None => None,
    };
    Ok(TurnReplyAddress {
        correlation_id,
        reply_to,
        deadline_at_ms,
    })
}

/// Errors from the turn request/reply helpers: either the persona
/// codec rejected the payload/headers, or the substrate send failed.
#[derive(Debug)]
pub enum PersonaTurnError {
    Codec(PersonaCodecError),
    Substrate(AircError),
}

impl std::fmt::Display for PersonaTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Codec(error) => write!(f, "persona turn codec failed: {error}"),
            Self::Substrate(error) => write!(f, "persona turn substrate send failed: {error}"),
        }
    }
}

impl std::error::Error for PersonaTurnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(error) => Some(error),
            Self::Substrate(error) => Some(error),
        }
    }
}

impl From<PersonaCodecError> for PersonaTurnError {
    fn from(error: PersonaCodecError) -> Self {
        Self::Codec(error)
    }
}

impl From<AircError> for PersonaTurnError {
    fn from(error: AircError) -> Self {
        Self::Substrate(error)
    }
}

/// Send a [`TurnRequested`] through the command-bus request path.
///
/// The persona codec headers (body hint, kind, persona/activity/turn
/// ids, optional model hint) ride alongside the substrate-stamped
/// `airc.correlation_id` / `airc.reply_to` / `airc.deadline`. Await
/// the turn with [`Airc::await_reply`] — it resolves with the
/// responder's [`TurnEmitted`] event, or errors with
/// `AircError::CommandDeadline` when the deadline elapses first.
pub async fn request_turn(
    airc: &Airc,
    target: MentionTarget,
    request: &TurnRequested,
    deadline: Duration,
) -> Result<PendingCommand, PersonaTurnError> {
    let (headers, body) = encode_persona_event(&PersonaEvent::TurnRequested(request.clone()))?;
    Ok(airc.request(target, headers, body, deadline).await?)
}

/// Reply to a command-bus [`TurnRequested`] with a [`TurnEmitted`],
/// echoing the request's correlation id so the requester's
/// [`Airc::await_reply`] resolves with this event. `request_headers`
/// are the headers off the request event as received.
pub async fn reply_turn_emitted(
    airc: &Airc,
    request_headers: &Headers,
    emitted: &TurnEmitted,
) -> Result<(), PersonaTurnError> {
    let address = turn_reply_address(request_headers)?;
    let (headers, body) = encode_persona_event(&PersonaEvent::TurnEmitted(emitted.clone()))?;
    airc.reply(address.reply_to, address.correlation_id, headers, body)
        .await?;
    Ok(())
}

/// A consumer-side filter that admits any persona event. Combine
/// with header constants via [`HeaderFilter::All`] to scope to a
/// specific persona or activity at subscribe time.
pub fn any_persona_event_filter() -> EventFilter {
    EventFilter {
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::Exact {
            key: HEADER_FORGE_BODY_HINT.to_string(),
            value: BODY_HINT_FORGE_PERSONA_EVENT.to_string(),
        },
    }
}

/// Scope to one activity: events whose body-hint is `forge.persona.event.v1`
/// AND whose `forge.continuum.activity_id` equals the given id.
pub fn activity_event_filter(activity_id: &str) -> EventFilter {
    EventFilter {
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::All(vec![
            HeaderFilter::Exact {
                key: HEADER_FORGE_BODY_HINT.to_string(),
                value: BODY_HINT_FORGE_PERSONA_EVENT.to_string(),
            },
            HeaderFilter::Exact {
                key: HEADER_FORGE_CONTINUUM_ACTIVITY_ID.to_string(),
                value: activity_id.to_string(),
            },
        ]),
    }
}
