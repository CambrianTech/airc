//! The spawned-persona runtime surface.
//!
//! Mirrors `embedded_consumer_smoke::agent`: deliberately tiny
//! product-adjacent code modeling what a Continuum persona process
//! needs from AIRC. Everything here rides public surfaces — `airc-lib`
//! for the peer handle, `airc-core` for the PersonaCapabilities
//! identity contract (card 9e5f8844), `airc-trust` for the
//! parent-tier pin, `consumer-shapes` for the PersonaEvent codec.

use std::error::Error;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, SystemTimeError, UNIX_EPOCH};

use airc_core::{Identity, PeerId, PersonaCapabilities, PersonaCapabilitiesError};
use airc_lib::{Airc, AircError, FilteredEventStream, PeerSpec};
use airc_trust::{PeersStoreError, TrustTier};
use consumer_shapes::continuum::{
    any_persona_event_filter, decode_persona_event, encode_persona_event, PersonaCodecError,
    PersonaEvent, TurnEmitted, TurnRequested,
};
use futures::StreamExt;

/// Where the persona's own identity home lives. Always REQUIRED — a
/// persona is a real peer with its own PeerId + identity.key, never a
/// sub-identity inside the parent's home.
pub const ENV_AIRC_HOME: &str = "AIRC_HOME";
/// The parent's `peer_id:pubkey` spec (the `Airc::peer_spec` string).
/// REQUIRED — the spawn relationship is the trust bootstrap.
pub const ENV_PARENT_PEER_SPEC: &str = "AIRC_PARENT_PEER_SPEC";
/// Room the persona serves turns in. Optional; defaults to
/// [`DEFAULT_PERSONA_ROOM`].
pub const ENV_PERSONA_ROOM: &str = "AIRC_PERSONA_ROOM";
/// Optional loopback/LAN address of the parent's listener. When set,
/// the binary dials it after spawn so turn traffic has a route
/// (in-process tests wire the link themselves).
pub const ENV_PARENT_LAN_ADDR: &str = "AIRC_PARENT_LAN_ADDR";
/// Optional persona id override for the capability advert.
pub const ENV_PERSONA_ID: &str = "AIRC_PERSONA_ID";

pub const DEFAULT_PERSONA_ROOM: &str = "persona-smoke";
pub const DEFAULT_PERSONA_ID: &str = "persona-smoke-echo";

/// Everything the spawn loop hands a persona process.
#[derive(Debug, Clone)]
pub struct PersonaAgentConfig {
    /// Identity home for THIS persona (own PeerId + identity.key).
    pub home: PathBuf,
    /// Parent's `peer_id:pubkey` spec string.
    pub parent_spec: String,
    /// Room to join and serve turns in.
    pub room: String,
    /// What this persona advertises on its identity card.
    pub capabilities: PersonaCapabilities,
    /// Parent LAN listener to dial after spawn, when the route isn't
    /// provided some other way (daemon, stored endpoints, relay).
    pub parent_lan_addr: Option<SocketAddr>,
}

impl PersonaAgentConfig {
    /// Build the config a spawned persona process reads from its
    /// environment. `AIRC_HOME` and `AIRC_PARENT_PEER_SPEC` are
    /// required; missing values fail loudly with the variable name.
    pub fn from_env() -> Result<Self, PersonaAgentError> {
        let home = require_env(ENV_AIRC_HOME)?;
        let parent_spec = require_env(ENV_PARENT_PEER_SPEC)?;
        let room = optional_env(ENV_PERSONA_ROOM).unwrap_or_else(|| DEFAULT_PERSONA_ROOM.into());
        let persona_id = optional_env(ENV_PERSONA_ID).unwrap_or_else(|| DEFAULT_PERSONA_ID.into());
        let parent_lan_addr = match optional_env(ENV_PARENT_LAN_ADDR) {
            Some(raw) => Some(raw.parse().map_err(|source| PersonaAgentError::BadEnv {
                name: ENV_PARENT_LAN_ADDR,
                raw: raw.clone(),
                detail: format!("{source}"),
            })?),
            None => None,
        };
        Ok(Self {
            home: PathBuf::from(home),
            parent_spec,
            room,
            capabilities: PersonaCapabilities {
                persona_id,
                capability_tags: vec!["echo".to_string(), "smoke".to_string()],
                model: "example-echo".to_string(),
                context_window_tokens: 8_192,
            },
            parent_lan_addr,
        })
    }
}

fn require_env(name: &'static str) -> Result<String, PersonaAgentError> {
    optional_env(name).ok_or(PersonaAgentError::MissingEnv { name })
}

fn optional_env(name: &'static str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if value.is_empty() => None,
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => None,
    }
}

/// Loud-failure error surface for the persona agent. Every step of
/// the spawn loop that can fail names what failed; nothing degrades
/// silently (airc is the agent's umbilical — see AGENTS doctrine).
#[derive(Debug)]
pub enum PersonaAgentError {
    /// A required environment variable was missing or empty.
    MissingEnv { name: &'static str },
    /// An environment variable was present but unparseable.
    BadEnv {
        name: &'static str,
        raw: String,
        detail: String,
    },
    /// Any airc-lib operation (open, add_peer, join, send, subscribe).
    /// Boxed: these foreign error enums are large, and an unboxed
    /// variant makes every `Result<_, PersonaAgentError>` on the hot
    /// path carry that whole size (clippy `result_large_err`). The cold
    /// error path can afford one allocation; the happy path stays small.
    Airc(Box<AircError>),
    /// The PersonaCapabilities identity write/read failed. Boxed (see
    /// [`Self::Airc`]).
    Capabilities(Box<PersonaCapabilitiesError>),
    /// A persona event failed to encode/decode. The subscription
    /// filter admits only `forge.persona.event.v1` bodies, so a
    /// decode failure here means a malformed event — surfaced, never
    /// skipped silently. Boxed (see [`Self::Airc`]).
    Codec(Box<PersonaCodecError>),
    /// The trust-store tier pin failed at the store layer. Boxed (see
    /// [`Self::Airc`]).
    Trust(Box<PeersStoreError>),
    /// `airc_trust::set_tier` reported the parent is not enrolled —
    /// a structural bug in the spawn sequence (enrol precedes pin).
    ParentNotEnrolled { parent: PeerId },
    /// System clock predates the Unix epoch.
    Clock(SystemTimeError),
    /// The live subscription lagged and dropped events.
    Lag(String),
    /// The live subscription ended — the persona's umbilical is gone.
    /// Loud by doctrine: a persona that can no longer hear turn
    /// requests must say so, not idle silently.
    SubscriptionClosed,
}

impl fmt::Display for PersonaAgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnv { name } => {
                write!(f, "required environment variable {name} is missing")
            }
            Self::BadEnv { name, raw, detail } => {
                write!(
                    f,
                    "environment variable {name}={raw:?} is invalid: {detail}"
                )
            }
            Self::Airc(source) => write!(f, "airc operation failed: {source}"),
            Self::Capabilities(source) => {
                write!(f, "persona capabilities identity write failed: {source}")
            }
            Self::Codec(source) => write!(f, "persona event codec failed: {source}"),
            Self::Trust(source) => write!(f, "parent tier pin failed: {source}"),
            Self::ParentNotEnrolled { parent } => write!(
                f,
                "parent {parent} is not enrolled in the trust store; \
                 enrolment must precede the tier pin",
            ),
            Self::Clock(source) => write!(f, "system clock error: {source}"),
            Self::Lag(detail) => write!(f, "live subscription lagged: {detail}"),
            Self::SubscriptionClosed => f.write_str("live persona-event subscription closed"),
        }
    }
}

impl Error for PersonaAgentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            // `&**source` derefs the Box to the concrete error, which
            // coerces to `&dyn Error` (a `&Box<E>` does not).
            Self::Airc(source) => Some(&**source),
            Self::Capabilities(source) => Some(&**source),
            Self::Codec(source) => Some(&**source),
            Self::Trust(source) => Some(&**source),
            Self::Clock(source) => Some(source),
            Self::MissingEnv { .. }
            | Self::BadEnv { .. }
            | Self::ParentNotEnrolled { .. }
            | Self::Lag(_)
            | Self::SubscriptionClosed => None,
        }
    }
}

impl From<AircError> for PersonaAgentError {
    fn from(source: AircError) -> Self {
        Self::Airc(Box::new(source))
    }
}

impl From<PersonaCapabilitiesError> for PersonaAgentError {
    fn from(source: PersonaCapabilitiesError) -> Self {
        Self::Capabilities(Box::new(source))
    }
}

impl From<PersonaCodecError> for PersonaAgentError {
    fn from(source: PersonaCodecError) -> Self {
        Self::Codec(Box::new(source))
    }
}

impl From<PeersStoreError> for PersonaAgentError {
    fn from(source: PeersStoreError) -> Self {
        Self::Trust(Box::new(source))
    }
}

impl From<SystemTimeError> for PersonaAgentError {
    fn from(source: SystemTimeError) -> Self {
        Self::Clock(source)
    }
}

/// A live spawned persona: its own `Airc` peer, capabilities
/// advertised, parent pinned at OwnMachine, room joined, persona-event
/// subscription installed and ready to serve turns.
pub struct PersonaAgent {
    airc: Airc,
    capabilities: PersonaCapabilities,
    parent: PeerId,
    inbox: FilteredEventStream,
}

impl PersonaAgent {
    /// Run the spawn sequence (steps 2–5 of the loop; see crate docs):
    /// open own home → advertise capabilities on the identity card →
    /// enrol parent → pin parent at `OwnMachine` → join the room →
    /// subscribe to persona events.
    ///
    /// The subscription is installed BEFORE `spawn` returns, so a
    /// parent may send `TurnRequested` as soon as it has a route to
    /// us — no race against a later subscribe.
    pub async fn spawn(config: PersonaAgentConfig) -> Result<Self, PersonaAgentError> {
        let airc = Airc::open(&config.home).await?;

        // Capabilities ride the EXISTING integrations map on the
        // identity card (card 9e5f8844) — no parallel persona registry.
        airc.set_local_identity_card(advertised_identity(&config.capabilities)?)
            .await?;

        // Trust bootstrap: enrol the parent's pinned key, then raise
        // it to OwnMachine — the spawn relationship is definitionally
        // the same-machine relationship (airc-trust tier doctrine).
        let parent_spec: PeerSpec = config.parent_spec.parse().map_err(AircError::from)?;
        let parent = parent_spec.peer_id;
        airc.add_peer(parent_spec).await?;
        let pinned = airc_trust::set_tier(airc.home(), parent, TrustTier::OwnMachine).await?;
        if pinned.is_none() {
            return Err(PersonaAgentError::ParentNotEnrolled { parent });
        }

        airc.join(&config.room).await?;
        let inbox = airc.subscribe_filtered(any_persona_event_filter()).await?;

        Ok(Self {
            airc,
            capabilities: config.capabilities,
            parent,
            inbox,
        })
    }

    pub fn airc(&self) -> &Airc {
        &self.airc
    }

    /// This persona's own `peer_id:pubkey` spec — what it reports back
    /// to the parent so the parent can enrol it in turn.
    pub fn peer_spec(&self) -> String {
        self.airc.peer_spec()
    }

    pub fn capabilities(&self) -> &PersonaCapabilities {
        &self.capabilities
    }

    pub fn parent_peer_id(&self) -> PeerId {
        self.parent
    }

    /// The identity card this persona advertises, capabilities
    /// included — what the parent reads back through the roster.
    pub fn identity_card(&self) -> Result<Identity, PersonaAgentError> {
        advertised_identity(&self.capabilities)
    }

    /// Dial the parent's LAN listener so turn traffic has a route.
    /// The substrate refuses to publish without an admissible
    /// cross-machine route, so a freshly spawned persona either dials
    /// (this), inherits stored endpoints, or attaches to a daemon.
    pub async fn connect_parent_lan(&self, addr: SocketAddr) -> Result<(), PersonaAgentError> {
        self.airc.connect_lan(addr, self.parent).await?;
        Ok(())
    }

    /// Serve one turn: wait (up to `timeout`) for a `TurnRequested`
    /// addressed at this persona's room, reply with `TurnEmitted`
    /// echoing the prompt, and return the reply that was sent.
    ///
    /// Returns `Ok(None)` when the deadline passes without a turn
    /// request (idle is normal weather). Malformed persona events,
    /// stream lag, and a closed subscription are loud errors, never
    /// skipped — the subscription IS this persona's umbilical.
    pub async fn serve_next_turn(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<TurnEmitted>, PersonaAgentError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = deadline.saturating_duration_since(now);
            let event = match tokio::time::timeout(remaining, self.inbox.next()).await {
                Ok(Some(Ok(event))) => event,
                Ok(Some(Err(lag))) => return Err(PersonaAgentError::Lag(lag.to_string())),
                Ok(None) => return Err(PersonaAgentError::SubscriptionClosed),
                Err(_elapsed) => return Ok(None),
            };
            // Self-echo: our own TurnEmitted replies match the filter.
            if event.peer_id == self.airc.peer_id() && event.client_id == self.airc.client_id() {
                continue;
            }
            let decoded = decode_persona_event(&event.headers, event.body.as_ref())?;
            let request = match decoded {
                PersonaEvent::TurnRequested(request) => request,
                PersonaEvent::TurnEmitted(_)
                | PersonaEvent::ActivityStarted(_)
                | PersonaEvent::ActivityEnded(_) => continue,
            };
            let reply = self.reply_to_turn(&request).await?;
            return Ok(Some(reply));
        }
    }

    /// Build + publish the `TurnEmitted` answer for one request.
    async fn reply_to_turn(
        &self,
        request: &TurnRequested,
    ) -> Result<TurnEmitted, PersonaAgentError> {
        let emitted = TurnEmitted {
            persona_id: self.capabilities.persona_id.clone(),
            activity_id: request.activity_id.clone(),
            turn_id: request.turn_id.clone(),
            text: format!("echo: {}", request.prompt),
            emitted_at_ms: now_ms()?,
        };
        let (headers, body) = encode_persona_event(&PersonaEvent::TurnEmitted(emitted.clone()))?;
        self.airc.send(body, headers).await?;
        Ok(emitted)
    }
}

/// The identity card a persona with these capabilities advertises:
/// nick = persona id, role tag, capabilities on the integrations map.
fn advertised_identity(capabilities: &PersonaCapabilities) -> Result<Identity, PersonaAgentError> {
    let mut identity = Identity::new(capabilities.persona_id.clone());
    identity.role = "continuum-persona".to_string();
    capabilities.write_to_identity(&mut identity)?;
    Ok(identity)
}

/// Wall-clock milliseconds since the Unix epoch, surfacing clock
/// errors instead of panicking.
pub fn now_ms() -> Result<u64, PersonaAgentError> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    Ok(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}
