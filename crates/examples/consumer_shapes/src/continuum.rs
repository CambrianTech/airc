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

use airc_lib::{Body, EventFilter, HeaderFilter, Headers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

pub const BODY_HINT_FORGE_PERSONA_EVENT: &str = "forge.persona.event.v1";

pub const HEADER_FORGE_PERSONA_KIND: &str = "forge.persona.kind";
pub const HEADER_FORGE_PERSONA_ID: &str = "forge.persona.id";
pub const HEADER_FORGE_CONTINUUM_ACTIVITY_ID: &str = "forge.continuum.activity_id";
pub const HEADER_FORGE_CONTINUUM_TURN_ID: &str = "forge.continuum.turn_id";

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
