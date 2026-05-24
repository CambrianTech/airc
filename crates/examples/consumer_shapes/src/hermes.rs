//! Hermes integration shape: agent command/event contracts.
//!
//! Hermes wires agents to tools. An agent issues a command; a tool
//! invocation runs; a result returns. Each step is a typed event on
//! the AIRC wire with headers carrying agent + command identifiers
//! so an orchestrator can subscribe to one agent's command stream,
//! one tool's invocation stream, or one command's full lifecycle.
//!
//! Wire-level body hint: `forge.hermes.event.v1`.
//!
//! Header conventions:
//!   - `forge.hermes.kind`       — event variant
//!   - `forge.hermes.agent_id`   — issuing/receiving agent
//!   - `forge.hermes.command_id` — correlates command → result
//!   - `forge.hermes.tool`       — tool/capability name (on invocations)

use airc_lib::{Body, EventFilter, HeaderFilter, Headers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

pub const BODY_HINT_FORGE_HERMES_EVENT: &str = "forge.hermes.event.v1";

pub const HEADER_FORGE_HERMES_KIND: &str = "forge.hermes.kind";
pub const HEADER_FORGE_HERMES_AGENT_ID: &str = "forge.hermes.agent_id";
pub const HEADER_FORGE_HERMES_COMMAND_ID: &str = "forge.hermes.command_id";
pub const HEADER_FORGE_HERMES_TOOL: &str = "forge.hermes.tool";

const HEADER_FORGE_BODY_HINT: &str = "forge.body_hint";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HermesEvent {
    /// An agent issued a command. May target a tool by name.
    AgentCommandIssued(AgentCommandIssued),
    /// A tool invocation completed (success or failure). Correlates
    /// to the matching `AgentCommandIssued` by `command_id`.
    AgentResultReturned(AgentResultReturned),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCommandIssued {
    pub agent_id: String,
    pub command_id: String,
    pub tool: String,
    /// Opaque tool input — encoded by the tool's own contract. Kept
    /// as `serde_json::Value` so the codec can roundtrip any shape.
    pub input: serde_json::Value,
    pub issued_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResultReturned {
    pub agent_id: String,
    pub command_id: String,
    pub tool: String,
    /// Either an `output` value (success) or an `error` string
    /// (failure). Honest partial-success is up to the tool: if it
    /// produced something AND failed, encode both in the `output`
    /// shape and leave `error` populated — never silently drop one.
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub returned_at_ms: u64,
}

impl HermesEvent {
    pub fn agent_id(&self) -> &str {
        match self {
            Self::AgentCommandIssued(e) => &e.agent_id,
            Self::AgentResultReturned(e) => &e.agent_id,
        }
    }
    pub fn command_id(&self) -> &str {
        match self {
            Self::AgentCommandIssued(e) => &e.command_id,
            Self::AgentResultReturned(e) => &e.command_id,
        }
    }
    pub fn tool(&self) -> &str {
        match self {
            Self::AgentCommandIssued(e) => &e.tool,
            Self::AgentResultReturned(e) => &e.tool,
        }
    }
    fn variant_kind(&self) -> &'static str {
        match self {
            Self::AgentCommandIssued(_) => "agent_command_issued",
            Self::AgentResultReturned(_) => "agent_result_returned",
        }
    }
}

#[derive(Debug)]
pub enum HermesCodecError {
    MissingBody,
    NonJsonBody,
    BodyHintMismatch {
        actual: Option<String>,
        expected: &'static str,
    },
    Json(serde_json::Error),
}

impl std::fmt::Display for HermesCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBody => f.write_str("hermes event body missing"),
            Self::NonJsonBody => f.write_str("hermes event body must be JSON"),
            Self::BodyHintMismatch { actual, expected } => write!(
                f,
                "hermes event body hint mismatch: actual={actual:?}, expected={expected:?}",
            ),
            Self::Json(error) => write!(f, "hermes event JSON codec failed: {error}"),
        }
    }
}

impl std::error::Error for HermesCodecError {}

impl From<serde_json::Error> for HermesCodecError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn encode_hermes_event(event: &HermesEvent) -> Result<(Headers, Body), HermesCodecError> {
    let mut headers = Headers::new();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_HERMES_EVENT.to_string(),
    );
    headers.insert(
        HEADER_FORGE_HERMES_KIND.to_string(),
        event.variant_kind().to_string(),
    );
    headers.insert(
        HEADER_FORGE_HERMES_AGENT_ID.to_string(),
        event.agent_id().to_string(),
    );
    headers.insert(
        HEADER_FORGE_HERMES_COMMAND_ID.to_string(),
        event.command_id().to_string(),
    );
    headers.insert(
        HEADER_FORGE_HERMES_TOOL.to_string(),
        event.tool().to_string(),
    );
    let body = Body::Json(serde_json::to_value(event)?);
    Ok((headers, body))
}

pub fn decode_hermes_event(
    headers: &Headers,
    body: Option<&Body>,
) -> Result<HermesEvent, HermesCodecError> {
    match headers.get(HEADER_FORGE_BODY_HINT) {
        Some(value) if value == BODY_HINT_FORGE_HERMES_EVENT => {}
        actual => {
            return Err(HermesCodecError::BodyHintMismatch {
                actual: actual.cloned(),
                expected: BODY_HINT_FORGE_HERMES_EVENT,
            });
        }
    }
    let body = body.ok_or(HermesCodecError::MissingBody)?;
    let Body::Json(value) = body else {
        return Err(HermesCodecError::NonJsonBody);
    };
    Ok(serde_json::from_value(value.clone())?)
}

pub fn any_hermes_event_filter() -> EventFilter {
    EventFilter {
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::Exact {
            key: HEADER_FORGE_BODY_HINT.to_string(),
            value: BODY_HINT_FORGE_HERMES_EVENT.to_string(),
        },
    }
}

/// Subscribe to one agent's command + result stream. The combination
/// of body-hint + agent_id is the routing primitive — body content
/// is opaque to the substrate.
pub fn agent_event_filter(agent_id: &str) -> EventFilter {
    EventFilter {
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::All(vec![
            HeaderFilter::Exact {
                key: HEADER_FORGE_BODY_HINT.to_string(),
                value: BODY_HINT_FORGE_HERMES_EVENT.to_string(),
            },
            HeaderFilter::Exact {
                key: HEADER_FORGE_HERMES_AGENT_ID.to_string(),
                value: agent_id.to_string(),
            },
        ]),
    }
}
