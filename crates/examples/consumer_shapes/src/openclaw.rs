//! OpenClaw integration shape: chat/thread identity bridge.
//!
//! OpenClaw has its own user identity model and thread/workspace
//! taxonomy that predates AIRC. The integration shape is a typed
//! adapter that carries the OpenClaw identifiers as headers
//! alongside the AIRC envelope, so a consumer can route events by
//! either system's notion of "who" and "where":
//!
//!   - OpenClaw user → AIRC `PeerId` (substrate identity), plus
//!     `forge.openclaw.user_id` retained as a header for any
//!     OpenClaw-aware subscriber.
//!   - OpenClaw thread → AIRC `RoomId` (substrate channel), plus
//!     `forge.openclaw.thread_id` retained.
//!   - OpenClaw workspace → header projected for routing; no
//!     substrate concept needed yet.
//!
//! Wire-level body hint: `forge.openclaw.event.v1`.

use airc_lib::{Body, EventFilter, HeaderFilter, Headers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

pub const BODY_HINT_FORGE_OPENCLAW_EVENT: &str = "forge.openclaw.event.v1";

pub const HEADER_FORGE_OPENCLAW_KIND: &str = "forge.openclaw.kind";
pub const HEADER_FORGE_OPENCLAW_USER_ID: &str = "forge.openclaw.user_id";
pub const HEADER_FORGE_OPENCLAW_THREAD_ID: &str = "forge.openclaw.thread_id";
pub const HEADER_FORGE_OPENCLAW_WORKSPACE_ID: &str = "forge.openclaw.workspace_id";

const HEADER_FORGE_BODY_HINT: &str = "forge.body_hint";

/// Representative OpenClaw events. Real integrations extend with
/// their own variants (mentions, reactions, edits, attachments...);
/// the shape below is the pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpenClawEvent {
    /// A user posted a chat message in a thread.
    ChatMessagePosted(ChatMessagePosted),
    /// A new thread was created within a workspace.
    ThreadCreated(ThreadCreated),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessagePosted {
    /// The OpenClaw user identifier. Stable across thread changes.
    pub openclaw_user_id: String,
    /// The OpenClaw thread identifier. Maps to an AIRC channel.
    pub openclaw_thread_id: String,
    /// The OpenClaw workspace identifier. Lets a cross-thread
    /// subscriber filter to one workspace.
    pub openclaw_workspace_id: String,
    pub text: String,
    pub posted_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCreated {
    pub openclaw_user_id: String,
    pub openclaw_thread_id: String,
    pub openclaw_workspace_id: String,
    pub title: String,
    pub created_at_ms: u64,
}

impl OpenClawEvent {
    pub fn user_id(&self) -> &str {
        match self {
            Self::ChatMessagePosted(e) => &e.openclaw_user_id,
            Self::ThreadCreated(e) => &e.openclaw_user_id,
        }
    }
    pub fn thread_id(&self) -> &str {
        match self {
            Self::ChatMessagePosted(e) => &e.openclaw_thread_id,
            Self::ThreadCreated(e) => &e.openclaw_thread_id,
        }
    }
    pub fn workspace_id(&self) -> &str {
        match self {
            Self::ChatMessagePosted(e) => &e.openclaw_workspace_id,
            Self::ThreadCreated(e) => &e.openclaw_workspace_id,
        }
    }
    fn variant_kind(&self) -> &'static str {
        match self {
            Self::ChatMessagePosted(_) => "chat_message_posted",
            Self::ThreadCreated(_) => "thread_created",
        }
    }
}

#[derive(Debug)]
pub enum OpenClawCodecError {
    MissingBody,
    NonJsonBody,
    BodyHintMismatch {
        actual: Option<String>,
        expected: &'static str,
    },
    Json(serde_json::Error),
}

impl std::fmt::Display for OpenClawCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBody => f.write_str("openclaw event body missing"),
            Self::NonJsonBody => f.write_str("openclaw event body must be JSON"),
            Self::BodyHintMismatch { actual, expected } => write!(
                f,
                "openclaw event body hint mismatch: actual={actual:?}, expected={expected:?}",
            ),
            Self::Json(error) => write!(f, "openclaw event JSON codec failed: {error}"),
        }
    }
}

impl std::error::Error for OpenClawCodecError {}

impl From<serde_json::Error> for OpenClawCodecError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn encode_openclaw_event(event: &OpenClawEvent) -> Result<(Headers, Body), OpenClawCodecError> {
    let mut headers = Headers::new();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_OPENCLAW_EVENT.to_string(),
    );
    headers.insert(
        HEADER_FORGE_OPENCLAW_KIND.to_string(),
        event.variant_kind().to_string(),
    );
    headers.insert(
        HEADER_FORGE_OPENCLAW_USER_ID.to_string(),
        event.user_id().to_string(),
    );
    headers.insert(
        HEADER_FORGE_OPENCLAW_THREAD_ID.to_string(),
        event.thread_id().to_string(),
    );
    headers.insert(
        HEADER_FORGE_OPENCLAW_WORKSPACE_ID.to_string(),
        event.workspace_id().to_string(),
    );
    let body = Body::Json(serde_json::to_value(event)?);
    Ok((headers, body))
}

pub fn decode_openclaw_event(
    headers: &Headers,
    body: Option<&Body>,
) -> Result<OpenClawEvent, OpenClawCodecError> {
    match headers.get(HEADER_FORGE_BODY_HINT) {
        Some(value) if value == BODY_HINT_FORGE_OPENCLAW_EVENT => {}
        actual => {
            return Err(OpenClawCodecError::BodyHintMismatch {
                actual: actual.cloned(),
                expected: BODY_HINT_FORGE_OPENCLAW_EVENT,
            });
        }
    }
    let body = body.ok_or(OpenClawCodecError::MissingBody)?;
    let Body::Json(value) = body else {
        return Err(OpenClawCodecError::NonJsonBody);
    };
    Ok(serde_json::from_value(value.clone())?)
}

pub fn any_openclaw_event_filter() -> EventFilter {
    EventFilter {
        // #1271 added EventFilter.self_echo; consumers see everything (None).
        self_echo: None,
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::Exact {
            key: HEADER_FORGE_BODY_HINT.to_string(),
            value: BODY_HINT_FORGE_OPENCLAW_EVENT.to_string(),
        },
    }
}

/// Scope by workspace — common cross-thread routing requirement.
pub fn workspace_event_filter(workspace_id: &str) -> EventFilter {
    EventFilter {
        // #1271 added EventFilter.self_echo; consumers see everything (None).
        self_echo: None,
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::All(vec![
            HeaderFilter::Exact {
                key: HEADER_FORGE_BODY_HINT.to_string(),
                value: BODY_HINT_FORGE_OPENCLAW_EVENT.to_string(),
            },
            HeaderFilter::Exact {
                key: HEADER_FORGE_OPENCLAW_WORKSPACE_ID.to_string(),
                value: workspace_id.to_string(),
            },
        ]),
    }
}
