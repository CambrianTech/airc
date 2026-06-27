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
    Airc, AircError, Body, CapabilityCandidate, CapabilityQuery, CapabilityRegistry, EventFilter,
    HeaderFilter, Headers, MentionTarget, PeerId, PendingCommand, PersonaCapabilities, TrustTier,
    HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_DEADLINE, HEADER_AIRC_REPLY_TO,
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
    /// A turn reply decoded as a persona event, but not the expected
    /// variant. A cross-grid inference reply MUST be a `TurnEmitted`; any
    /// other variant on the correlation is a protocol violation surfaced
    /// loudly rather than silently dropped.
    UnexpectedReplyVariant {
        actual: &'static str,
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
            Self::MissingHeader { header } => {
                write!(f, "persona turn request missing required header {header:?}")
            }
            Self::MalformedHeader { header, value } => {
                write!(
                    f,
                    "persona turn request header {header:?} malformed: {value:?}"
                )
            }
            Self::UnexpectedReplyVariant { actual, expected } => write!(
                f,
                "persona turn reply variant mismatch: actual={actual:?}, expected={expected:?}",
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

// ---------------------------------------------------------------------------
// Capability offer + registry routing — card a9580f9d (persona-peer 4/8)
// ---------------------------------------------------------------------------
//
// The organic two-sided matcher. A persona node ADVERTISES what it can
// do (a capability offer carrying its card-9e5f8844 `PersonaCapabilities`
// + its `peer_id`); a scheduler expressing a NEED matches against those
// offers via [`airc_lib::CapabilityRegistry`]. This is the escalation
// half of local-first routing: try the local node first, consult the
// registry only when local can't meet the need. Grid-of-one is normal —
// a node with no peers ingests no offers and the registry stays
// empty-but-valid.

/// Body hint for a capability offer. Versioned independently of the
/// persona-event hint: an offer is a distinct wire shape (a standing
/// advert, not an activity event), so it gets its own additive `v1`
/// rather than becoming a `PersonaEvent` variant.
pub const BODY_HINT_FORGE_PERSONA_CAPABILITY_OFFER: &str = "forge.persona.capability_offer.v1";

/// Header projecting the offering node's peer id, so a scheduler can
/// filter offers by source without decoding the body. Consumer-owned
/// `forge.persona.*` namespace.
pub const HEADER_FORGE_PERSONA_PEER_ID: &str = "forge.persona.peer_id";

/// A node's standing capability advert, published to the room.
///
/// Reuses card 9e5f8844's [`PersonaCapabilities`] verbatim (the WHAT)
/// and pairs it with the offering [`PeerId`] (the WHO). Additive +
/// versioned like the `model_hint` field was: a future shape gets a new
/// body hint, never a silent change to `v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityOffer {
    /// The node making the offer. Registry candidates key off this.
    pub peer_id: PeerId,
    /// Exactly what this node advertises about itself (card 9e5f8844).
    pub capabilities: PersonaCapabilities,
    /// When the offer was produced (epoch-ms). The registry's ageing
    /// clock is fed from this — a node that keeps re-advertising stays
    /// live, one that goes quiet ages out.
    pub offered_at_ms: u64,
}

/// Encode a [`CapabilityOffer`] to `(Headers, Body)` for
/// [`airc_lib::Airc::send`]. Projects the body hint + offering peer id
/// as headers so subscribers filter without decoding the body — same
/// pattern as [`encode_persona_event`].
pub fn encode_capability_offer(
    offer: &CapabilityOffer,
) -> Result<(Headers, Body), PersonaCodecError> {
    let mut headers = Headers::new();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_PERSONA_CAPABILITY_OFFER.to_string(),
    );
    headers.insert(
        HEADER_FORGE_PERSONA_PEER_ID.to_string(),
        offer.peer_id.to_string(),
    );
    headers.insert(
        HEADER_FORGE_PERSONA_ID.to_string(),
        offer.capabilities.persona_id.clone(),
    );
    let body = Body::Json(serde_json::to_value(offer)?);
    Ok((headers, body))
}

/// Decode a [`CapabilityOffer`] off an event's headers + body. Loud on a
/// body-hint mismatch (an offer mis-read as something else would silently
/// never enter the registry, dropping a capable node from routing).
pub fn decode_capability_offer(
    headers: &Headers,
    body: Option<&Body>,
) -> Result<CapabilityOffer, PersonaCodecError> {
    match headers.get(HEADER_FORGE_BODY_HINT) {
        Some(value) if value == BODY_HINT_FORGE_PERSONA_CAPABILITY_OFFER => {}
        actual => {
            return Err(PersonaCodecError::BodyHintMismatch {
                actual: actual.cloned(),
                expected: BODY_HINT_FORGE_PERSONA_CAPABILITY_OFFER,
            });
        }
    }
    let body = body.ok_or(PersonaCodecError::MissingBody)?;
    let Body::Json(value) = body else {
        return Err(PersonaCodecError::NonJsonBody);
    };
    Ok(serde_json::from_value(value.clone())?)
}

/// Subscription filter admitting capability offers (and only those).
pub fn capability_offer_filter() -> EventFilter {
    EventFilter {
        // #1271 added EventFilter.self_echo; consumers see everything (None).
        self_echo: None,
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::Exact {
            key: HEADER_FORGE_BODY_HINT.to_string(),
            value: BODY_HINT_FORGE_PERSONA_CAPABILITY_OFFER.to_string(),
        },
    }
}

/// Ingest a decoded [`CapabilityOffer`] into a [`CapabilityRegistry`].
/// The bridge from the wire event (consumer-shapes layer) to the
/// wire-agnostic projection (airc-lib layer): the registry holds
/// `PersonaCapabilities` keyed by `peer_id`, never the wire envelope.
pub fn ingest_capability_offer(registry: &mut CapabilityRegistry, offer: &CapabilityOffer) {
    registry.ingest_offer(
        offer.peer_id,
        offer.capabilities.clone(),
        offer.offered_at_ms,
    );
}

/// Pick the best registry candidate to route a [`TurnRequested`] to,
/// given its `model_hint` (card ee2a339f) and/or extra required tags.
///
/// This is the bridge between 3/8's `forge.persona.model_hint` header
/// and 4/8's actual selection. A `model_hint`, when present, is treated
/// as one more required capability tag — a node advertising it (in its
/// `capability_tags`) is preferred. With no hint and no extra tags, this
/// degrades to a "best available node" sweep (highest trust, then widest
/// context window) via the empty-tags path.
///
/// Returns `None` (never an error) when nothing matches — grid-of-one,
/// or no node advertises the hinted model. The caller then runs locally
/// or queues; selection does not editorialise.
pub fn select_candidate_for_turn(
    registry: &CapabilityRegistry,
    request: &TurnRequested,
    extra_required_tags: &[&str],
    now_ms: u64,
    ttl_ms: u64,
    trust_of: impl Fn(PeerId) -> TrustTier,
) -> Option<CapabilityCandidate> {
    let mut required: Vec<&str> = extra_required_tags.to_vec();
    if let Some(hint) = request.model_hint.as_deref() {
        if !required.contains(&hint) {
            required.push(hint);
        }
    }
    let query = CapabilityQuery {
        required_tags: &required,
        now_ms,
        ttl_ms,
        // A real continuum consumer passes the adapter-ladder-reachable
        // peer set here (connected_lan_peers + recent direct frames) so a
        // LAN-reachable peer is never dropped from routing on a stale
        // beacon; this example has no live route state, so None (the prior
        // beacon-only behaviour).
        reachable_peers: None,
    };
    registry.match_for(&query, trust_of).into_iter().next()
}

// ---------------------------------------------------------------------------
// Local-first inference routing — card cae4bab1 (persona-peer 8/8)
// ---------------------------------------------------------------------------
//
// The protocol spine for cross-grid inference. THE binding principle (Joel):
// a requesting node tries its OWN local capability FIRST and only escalates
// to the mesh (the cross-grid request/reply path) when local cannot meet the
// need. This module owns the decision (`resolve_inference_target`) and the
// escalation helper (`request_inference_remote`); it adds NO new substrate
// primitives — it composes 3/8's `request_turn` / `Airc::await_reply` with
// 4/8's `select_candidate_for_turn`.
//
// Layering: the local capability is an airc-core `PersonaCapabilities`, the
// registry is airc-lib, the turn types are consumer-shapes. All three already
// compose here (exactly where `select_candidate_for_turn` lives), so the
// decision belongs here too — no dependency is inverted.

/// Where a turn should run, decided LOCAL-FIRST.
///
/// `resolve_inference_target` returns this; the caller branches on it. The
/// shape is deliberately three-valued and exhaustive so a grid-of-one with
/// no capable node is a *typed* outcome the caller must handle, never a
/// silent hang or an unwrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceTarget {
    /// This node can satisfy the turn itself — run it in-process, emit NO
    /// cross-grid request. The preferred outcome whenever local can meet
    /// the need.
    Local,
    /// No local match, but a mesh peer advertises the needed capability.
    /// The caller escalates via [`request_inference_remote`] to this peer.
    Remote(CapabilityCandidate),
    /// Neither local nor any live remote can meet the need (grid-of-one,
    /// or a model nobody advertises). A LOUD typed terminal — the caller
    /// surfaces it, never blocks waiting for a reply that cannot come.
    Unavailable,
}

/// Does the local node's own capability card satisfy this turn's need?
///
/// The need is the union of the turn's `model_hint` (treated as a required
/// tag, identically to [`select_candidate_for_turn`]) and any
/// `extra_required_tags`. Local satisfies it iff it advertises ALL of them.
/// An empty need (no hint, no extra tags) is satisfied by any local persona
/// — "run it yourself" is always valid when nothing specific is required.
fn local_satisfies(
    local: &PersonaCapabilities,
    request: &TurnRequested,
    extra_required_tags: &[&str],
) -> bool {
    let hint = request.model_hint.as_deref();
    let needed = extra_required_tags
        .iter()
        .copied()
        .chain(hint)
        .collect::<Vec<&str>>();
    needed.iter().all(|need| {
        local
            .capability_tags
            .iter()
            .any(|have| have.as_str() == *need)
    })
}

/// Decide where a turn runs, LOCAL-FIRST.
///
/// 1. If `local` advertises every required tag (model hint + extras),
///    return [`InferenceTarget::Local`] — no registry consultation, no
///    network request.
/// 2. Otherwise consult the cross-grid [`CapabilityRegistry`] via
///    [`select_candidate_for_turn`]; a match returns
///    [`InferenceTarget::Remote`].
/// 3. No local AND no remote → [`InferenceTarget::Unavailable`], a loud
///    typed terminal (the grid-of-one with no capable node).
///
/// `local` is `None` when this node hosts no persona at all (a pure
/// requester / relay); then step 1 is skipped and it goes straight to the
/// registry. This is a pure decision: it emits nothing, so it cannot hang.
#[allow(clippy::too_many_arguments)]
pub fn resolve_inference_target(
    local: Option<&PersonaCapabilities>,
    registry: &CapabilityRegistry,
    request: &TurnRequested,
    extra_required_tags: &[&str],
    now_ms: u64,
    ttl_ms: u64,
    trust_of: impl Fn(PeerId) -> TrustTier,
) -> InferenceTarget {
    if let Some(local) = local {
        if local_satisfies(local, request, extra_required_tags) {
            return InferenceTarget::Local;
        }
    }
    match select_candidate_for_turn(
        registry,
        request,
        extra_required_tags,
        now_ms,
        ttl_ms,
        trust_of,
    ) {
        Some(candidate) => InferenceTarget::Remote(candidate),
        None => InferenceTarget::Unavailable,
    }
}

/// Run the cross-grid request/reply against a [`InferenceTarget::Remote`]
/// candidate and await its [`TurnEmitted`] reply within `deadline`.
///
/// Composes the existing primitives, adding none: 3/8's [`request_turn`]
/// (which rides [`Airc::request`], stamping correlation/reply-to/deadline)
/// directs the turn at the chosen peer; [`Airc::await_reply`] resolves with
/// the responder's reply event, deadline-bounded by construction; the typed
/// [`TurnEmitted`] is decoded back out. A deadline elapsing surfaces as
/// `AircError::CommandDeadline` through [`PersonaTurnError::Substrate`] —
/// loud, never a hang.
pub async fn request_inference_remote(
    airc: &Airc,
    candidate: &CapabilityCandidate,
    request: &TurnRequested,
    deadline: Duration,
) -> Result<TurnEmitted, PersonaTurnError> {
    let pending = request_turn(
        airc,
        MentionTarget::Peer(candidate.peer_id),
        request,
        deadline,
    )
    .await?;
    let reply = airc.await_reply(pending).await?;
    match decode_persona_event(&reply.headers, reply.body.as_ref())? {
        PersonaEvent::TurnEmitted(emitted) => Ok(emitted),
        other @ (PersonaEvent::TurnRequested(_)
        | PersonaEvent::ActivityStarted(_)
        | PersonaEvent::ActivityEnded(_)) => Err(PersonaTurnError::Codec(
            PersonaCodecError::UnexpectedReplyVariant {
                actual: other.variant_kind(),
                expected: "turn_emitted",
            },
        )),
    }
}

/// A consumer-side filter that admits any persona event. Combine
/// with header constants via [`HeaderFilter::All`] to scope to a
/// specific persona or activity at subscribe time.
pub fn any_persona_event_filter() -> EventFilter {
    EventFilter {
        // #1271 added EventFilter.self_echo; consumers see everything (None).
        self_echo: None,
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
        // #1271 added EventFilter.self_echo; consumers see everything (None).
        self_echo: None,
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

// ---------------------------------------------------------------------------
// Embedding service shape — the `ai/embedding` capability (BigMama 5090 facility)
// ---------------------------------------------------------------------------
//
// `EmbeddingRequested` / `EmbeddingEmitted` are the request/reply pair for the
// `ai/embedding` capability, riding the SAME command-bus carriage as
// `TurnRequested`/`TurnEmitted`: a consumer (`GridEmbeddingProvider`, slice 3)
// calls [`request_embedding_remote`] against a facility candidate; the bridge
// (`integrations/embedding-facility`) answers with [`reply_embedding_emitted`].
// Distinct wire family from persona events (an embedding is a property of
// CONTENT, not a persona), so it gets its own body hint, not a `PersonaEvent`
// variant — same reasoning as `CapabilityOffer`.
//
// Wire body hint: `forge.ai.embedding.v1`. The **model slug** is projected to a
// header so a responder filters by vector-space identity without decoding the
// body. That slug is the SAME identity used three ways — "identity is the
// model, transport is the policy": the routing URI fragment
// (`ai/embedding/<slug>`), the cache `provider_id` (`<slug>`), and this
// envelope header all carry the model, never the transport. Locking them to one
// derived slug is what keeps a locally-embedded vector and a grid-embedded one
// in the same space + the same cache entry.

/// Body hint for the embedding request/reply family. Versioned independently of
/// the persona-event + capability-offer hints (distinct wire shape → own `v1`).
pub const BODY_HINT_FORGE_AI_EMBEDDING: &str = "forge.ai.embedding.v1";

/// Header projecting the event variant (`requested` | `emitted`) for a cheap
/// filter without decoding the body.
pub const HEADER_FORGE_AI_EMBEDDING_KIND: &str = "forge.ai.embedding.kind";

/// Header projecting the embedder **model slug** — the vector-space identity.
/// A responder filters/refuses by this without decoding; it is the same slug as
/// the routing URI fragment and the cache `provider_id`.
pub const HEADER_FORGE_AI_EMBEDDING_MODEL: &str = "forge.ai.embedding.model";

/// Header projecting the request id so a reply correlates at the application
/// layer too (the substrate also stamps `airc.correlation_id`; this is the
/// human/debug-visible echo).
pub const HEADER_FORGE_AI_EMBEDDING_REQUEST_ID: &str = "forge.ai.embedding.request_id";

/// The embedding request/reply family. Mirrors [`PersonaEvent`]'s shape (tagged
/// enum + typed noun structs + header projection) for a different domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmbeddingEvent {
    /// A request to embed one or more inputs in a specific model's space.
    Requested(EmbeddingRequested),
    /// The vectors produced for a prior [`EmbeddingRequested`].
    Emitted(EmbeddingEmitted),
}

/// Ask a facility to embed `inputs` in `model`'s vector space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingRequested {
    /// Correlates the reply at the application layer.
    pub request_id: String,
    /// The embedder model slug — the vector-space identity. A responder that
    /// does not host this model MUST refuse (loud), never embed in another
    /// space. Same slug as the routing URI fragment + cache `provider_id`.
    pub model: String,
    /// One or more inputs to embed; the reply carries one vector per input,
    /// positionally aligned.
    pub inputs: Vec<String>,
    pub requested_at_ms: u64,
}

/// The vectors a facility produced for an [`EmbeddingRequested`]. (No `Eq`: it
/// carries `f32` vectors.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingEmitted {
    /// Echoes the request's `request_id`.
    pub request_id: String,
    /// The model the vectors are in — MUST equal the request's `model`. The
    /// consumer checks this before caching, so a space mismatch is caught, not
    /// silently mis-keyed.
    pub model: String,
    /// One vector per input, in input order.
    pub vectors: Vec<Vec<f32>>,
    /// Vector dimension (redundant with `vectors[..].len()` but explicit on the
    /// wire so a consumer validates without inspecting every row).
    pub dim: u32,
    pub emitted_at_ms: u64,
}

impl EmbeddingEvent {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Requested(e) => &e.request_id,
            Self::Emitted(e) => &e.request_id,
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Self::Requested(e) => &e.model,
            Self::Emitted(e) => &e.model,
        }
    }

    fn variant_kind(&self) -> &'static str {
        match self {
            Self::Requested(_) => "requested",
            Self::Emitted(_) => "emitted",
        }
    }
}

/// Encode an [`EmbeddingEvent`] to `(Headers, Body)`. Projects body hint, kind,
/// model slug, and request id as headers — same pattern as
/// [`encode_persona_event`].
pub fn encode_embedding_event(
    event: &EmbeddingEvent,
) -> Result<(Headers, Body), PersonaCodecError> {
    let mut headers = Headers::new();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_AI_EMBEDDING.to_string(),
    );
    headers.insert(
        HEADER_FORGE_AI_EMBEDDING_KIND.to_string(),
        event.variant_kind().to_string(),
    );
    headers.insert(
        HEADER_FORGE_AI_EMBEDDING_MODEL.to_string(),
        event.model().to_string(),
    );
    headers.insert(
        HEADER_FORGE_AI_EMBEDDING_REQUEST_ID.to_string(),
        event.request_id().to_string(),
    );
    let body = Body::Json(serde_json::to_value(event)?);
    Ok((headers, body))
}

/// Decode an [`EmbeddingEvent`] off headers + body. Loud on a body-hint
/// mismatch (an embedding frame mis-read as something else would drop a reply).
pub fn decode_embedding_event(
    headers: &Headers,
    body: Option<&Body>,
) -> Result<EmbeddingEvent, PersonaCodecError> {
    match headers.get(HEADER_FORGE_BODY_HINT) {
        Some(value) if value == BODY_HINT_FORGE_AI_EMBEDDING => {}
        actual => {
            return Err(PersonaCodecError::BodyHintMismatch {
                actual: actual.cloned(),
                expected: BODY_HINT_FORGE_AI_EMBEDDING,
            });
        }
    }
    let body = body.ok_or(PersonaCodecError::MissingBody)?;
    let Body::Json(value) = body else {
        return Err(PersonaCodecError::NonJsonBody);
    };
    Ok(serde_json::from_value(value.clone())?)
}

/// A consumer-side filter admitting any embedding event (e.g. the facility
/// bridge subscribing only to embedding traffic).
pub fn any_embedding_event_filter() -> EventFilter {
    EventFilter {
        // #1271 added EventFilter.self_echo; consumers see everything (None).
        self_echo: None,
        channel: None,
        channels: HashSet::new(),
        kinds: BTreeSet::new(),
        headers_filter: HeaderFilter::Exact {
            key: HEADER_FORGE_BODY_HINT.to_string(),
            value: BODY_HINT_FORGE_AI_EMBEDDING.to_string(),
        },
    }
}

/// Send an [`EmbeddingRequested`] through the command-bus request path. Await
/// with [`Airc::await_reply`] — it resolves with the responder's
/// [`EmbeddingEmitted`] (see [`request_embedding_remote`] for the typed
/// convenience), or errors `AircError::CommandDeadline` on timeout.
pub async fn request_embedding(
    airc: &Airc,
    target: MentionTarget,
    request: &EmbeddingRequested,
    deadline: Duration,
) -> Result<PendingCommand, PersonaTurnError> {
    let (headers, body) = encode_embedding_event(&EmbeddingEvent::Requested(request.clone()))?;
    Ok(airc.request(target, headers, body, deadline).await?)
}

/// Reply to an [`EmbeddingRequested`] with an [`EmbeddingEmitted`], echoing the
/// request's correlation so the requester's [`Airc::await_reply`] resolves.
pub async fn reply_embedding_emitted(
    airc: &Airc,
    request_headers: &Headers,
    emitted: &EmbeddingEmitted,
) -> Result<(), PersonaTurnError> {
    let address = turn_reply_address(request_headers)?;
    let (headers, body) = encode_embedding_event(&EmbeddingEvent::Emitted(emitted.clone()))?;
    airc.reply(address.reply_to, address.correlation_id, headers, body)
        .await?;
    Ok(())
}

/// Run the cross-grid embedding request/reply against a facility candidate and
/// await its [`EmbeddingEmitted`] within `deadline`. The embedding-domain twin
/// of [`request_inference_remote`]; this is what slice 3's `GridEmbeddingProvider`
/// calls on a cache miss it routes to the grid.
pub async fn request_embedding_remote(
    airc: &Airc,
    candidate: &CapabilityCandidate,
    request: &EmbeddingRequested,
    deadline: Duration,
) -> Result<EmbeddingEmitted, PersonaTurnError> {
    let pending = request_embedding(
        airc,
        MentionTarget::Peer(candidate.peer_id),
        request,
        deadline,
    )
    .await?;
    let reply = airc.await_reply(pending).await?;
    match decode_embedding_event(&reply.headers, reply.body.as_ref())? {
        EmbeddingEvent::Emitted(emitted) => Ok(emitted),
        EmbeddingEvent::Requested(_) => Err(PersonaTurnError::Codec(
            PersonaCodecError::UnexpectedReplyVariant {
                actual: "requested",
                expected: "emitted",
            },
        )),
    }
}
