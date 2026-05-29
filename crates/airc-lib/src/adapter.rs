//! Consumer adapter infrastructure — the pluggable surface that
//! continuum, hermes, openclaw, and future consumers plug into.
//!
//! Card 9c63f3d8. Joel directive (2026-05-29):
//!
//! > you dont CODE continuum into airc. you provide an adapter
//! > pluggable infra for continuum, hermes, and openclaw
//!
//! ## Why this exists
//!
//! Before this module, the embedding shape for a consumer was either
//! "shell out to the `airc` CLI" or "write your own bridge daemon."
//! Both produce three near-identical bridges (one per consumer), each
//! re-deriving the same envelope-routing logic. That's the wrong
//! shape: airc should ship the routing infrastructure once, and each
//! consumer should ship a thin adapter that says "I claim envelopes
//! with body hint X, here's how to interpret them."
//!
//! ## What the adapter does
//!
//! A [`ConsumerAdapter`] declares:
//!
//! - **identity**: a stable name (`"continuum"`, `"hermes"`,
//!   `"openclaw"`) used for logging + registry uniqueness.
//! - **body_hint**: which envelopes are addressed to this consumer.
//!   Continuum claims `"forge.persona.event.v1"`; hermes claims its
//!   own; openclaw its own. The airc subscribe loop routes incoming
//!   envelopes to the adapter whose body_hint matches.
//! - **`on_envelope`**: the typed handler the airc daemon calls when
//!   a matching envelope arrives. The adapter translates from airc's
//!   typed envelope into the consumer's domain shape and dispatches
//!   to consumer-internal handlers however that consumer wants
//!   (Commands.execute, Events.emit, direct callbacks, …).
//!
//! That's the whole contract. Emit-side adapters get for free via
//! [`Airc::publish`] — no need to override unless they need custom
//! header projection.
//!
//! ## Substrate-vs-consumer doctrine
//!
//! Airc routes envelopes; consumers interpret them. The adapter
//! trait NEVER knows what `forge.persona.event.v1` MEANS — that's
//! continuum's job. Airc only knows "this envelope has body hint X,
//! whoever claimed X gets called." Same doctrine as the wall
//! (#1045), task-negotiation headers (#1061), TrustTier (#1071+#1072):
//! substrate ships the mechanism, consumers ship the schema.
//!
//! ## Multi-adapter coexistence
//!
//! One airc daemon per machine; multiple adapters can plug in. The
//! same operator might run continuum (persona events) AND hermes
//! (planning events) on the same machine — both adapters register,
//! both receive their respective envelope traffic. No conflict;
//! body_hint disambiguates.
//!
//! ## What's NOT in this PR
//!
//! - Continuum/hermes/openclaw adapter implementations — those live
//!   in their respective consumer codebases as small (~50 LOC each)
//!   `impl ConsumerAdapter`.
//! - Daemon-side subscribe-loop wiring that calls `on_envelope` —
//!   shipped here as the registry surface (`Airc::register_adapter`),
//!   but the actual hot-path dispatch in the daemon's event loop is
//!   a follow-up card. This PR ships the *contract* + an in-process
//!   smoke test that proves the trait + registry shape works
//!   end-to-end; the daemon wiring uses the same registry.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use thiserror::Error;

use airc_core::TranscriptEvent;

/// Errors an adapter can return from [`ConsumerAdapter::on_envelope`].
///
/// Adapters that need to surface a domain-specific failure wrap it
/// in [`AdapterError::Consumer`] with a string explanation; airc
/// itself only cares whether the call succeeded for logging /
/// retry-decision purposes.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// Adapter rejected the envelope as malformed or in a state it
    /// can't handle. Non-fatal; the daemon logs and continues to
    /// route other envelopes to other adapters.
    #[error("consumer adapter rejected envelope: {0}")]
    Consumer(String),

    /// Adapter's internal IO failed (downstream socket dead,
    /// in-process handler panicked, …). Airc logs but does not
    /// attempt recovery; consumer is responsible for its own
    /// resilience.
    #[error("consumer adapter IO failure: {0}")]
    Io(String),
}

/// The pluggable contract consumers implement to receive airc traffic.
///
/// See module docs for the full design intent. Minimal example:
///
/// ```ignore
/// use airc_lib::adapter::{ConsumerAdapter, AdapterError};
/// use airc_lib::TranscriptEvent;
///
/// struct ContinuumAdapter { /* consumer-internal handle */ }
///
/// #[async_trait::async_trait]
/// impl ConsumerAdapter for ContinuumAdapter {
///     fn name(&self) -> &'static str { "continuum" }
///     fn body_hint(&self) -> &'static str { "forge.persona.event.v1" }
///     async fn on_envelope(&self, env: TranscriptEvent) -> Result<(), AdapterError> {
///         // translate env into a PersonaEvent and dispatch
///         // through continuum's Commands.execute / Events.emit
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait ConsumerAdapter: Send + Sync {
    /// Stable consumer name. Used for registry uniqueness, log
    /// prefixes, and adapter introspection. Must not change across
    /// runs of the same adapter.
    fn name(&self) -> &'static str;

    /// Body hint this adapter claims. Envelopes with a matching body
    /// hint header get routed to this adapter's [`Self::on_envelope`].
    /// Multiple adapters with the same body_hint is a registry
    /// error (see [`AdapterRegistry::register`]).
    fn body_hint(&self) -> &'static str;

    /// Inbound dispatch — airc hands the adapter a typed envelope.
    /// Adapter translates and dispatches to consumer-internal
    /// handlers however it wants.
    ///
    /// Returning `Err` is non-fatal at the airc layer; airc logs the
    /// failure and continues routing other envelopes. Consumers that
    /// need transactional semantics ("this envelope MUST be
    /// processed or the next call retries it") build that on their
    /// own side (durable inbox, ack handshake, etc.) — airc's
    /// substrate contract is at-least-once-from-store, not
    /// exactly-once.
    async fn on_envelope(&self, envelope: TranscriptEvent) -> Result<(), AdapterError>;
}

/// In-process registry of [`ConsumerAdapter`] instances, keyed by
/// adapter name (NOT body_hint — multiple adapters might one day
/// claim overlapping hints with different precedence rules; keying
/// by name keeps the registry's invariants simple).
///
/// `register` enforces:
/// - One adapter per name (re-registering the same name is rejected;
///   call [`Self::deregister`] first if the consumer needs to swap).
/// - One adapter per body_hint (two adapters claiming the same hint
///   would create ambiguous routing; rejected at register time).
///
/// Concurrency: backed by `RwLock<HashMap<…>>`. Reads (dispatch hot
/// path) take the read lock; registration / deregistration take the
/// write lock. Adapters are stored as `Arc<dyn ConsumerAdapter>` so
/// in-flight `on_envelope` calls survive a concurrent deregister.
#[derive(Default)]
pub struct AdapterRegistry {
    by_name: RwLock<HashMap<&'static str, Arc<dyn ConsumerAdapter>>>,
}

/// Errors registering / dispatching adapters.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("adapter named {0:?} is already registered")]
    DuplicateName(&'static str),

    #[error(
        "adapter {new_name:?} claims body_hint {body_hint:?}, but adapter \
         {existing_name:?} already claims it"
    )]
    DuplicateBodyHint {
        existing_name: &'static str,
        new_name: &'static str,
        body_hint: &'static str,
    },

    #[error("no adapter named {0:?} is registered")]
    UnknownName(&'static str),
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a consumer adapter. Rejects duplicate names and
    /// duplicate body_hints.
    pub fn register(&self, adapter: Arc<dyn ConsumerAdapter>) -> Result<(), RegistryError> {
        let name = adapter.name();
        let body_hint = adapter.body_hint();
        let mut by_name = self
            .by_name
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if by_name.contains_key(name) {
            return Err(RegistryError::DuplicateName(name));
        }
        for existing in by_name.values() {
            if existing.body_hint() == body_hint {
                return Err(RegistryError::DuplicateBodyHint {
                    existing_name: existing.name(),
                    new_name: name,
                    body_hint,
                });
            }
        }
        by_name.insert(name, adapter);
        Ok(())
    }

    /// Remove a registered adapter by name. Returns the removed
    /// adapter if present, `Ok(None)` if it was never registered.
    pub fn deregister(
        &self,
        name: &'static str,
    ) -> Result<Option<Arc<dyn ConsumerAdapter>>, RegistryError> {
        let mut by_name = self
            .by_name
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(by_name.remove(name))
    }

    /// Snapshot of registered adapter names. Stable order
    /// (alphabetical) so log lines and tests are deterministic.
    pub fn registered_names(&self) -> Vec<&'static str> {
        let by_name = self
            .by_name
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut names: Vec<&'static str> = by_name.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Look up the adapter claiming `body_hint`. Returns `None` if
    /// no adapter has registered for this hint; the daemon's dispatch
    /// loop should log + drop (or buffer for later, if its policy
    /// says so) when this returns None.
    pub fn adapter_for_body_hint(&self, body_hint: &str) -> Option<Arc<dyn ConsumerAdapter>> {
        let by_name = self
            .by_name
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        by_name
            .values()
            .find(|a| a.body_hint() == body_hint)
            .cloned()
    }

    /// Dispatch an envelope to the adapter whose body_hint matches
    /// the envelope's. Returns:
    /// - `Ok(true)` — adapter ran (may have returned its own Err,
    ///   which is propagated).
    /// - `Ok(false)` — no adapter claims this body_hint; daemon
    ///   decides whether to log/drop/buffer.
    ///
    /// The body_hint extraction looks at the standard
    /// `airc.body_hint` header. Envelopes without that header
    /// return `Ok(false)` (no consumer claimed them).
    pub async fn dispatch(&self, envelope: TranscriptEvent) -> Result<bool, AdapterError> {
        let Some(hint) = envelope.headers.get(HEADER_AIRC_BODY_HINT) else {
            return Ok(false);
        };
        let Some(adapter) = self.adapter_for_body_hint(hint.as_str()) else {
            return Ok(false);
        };
        adapter.on_envelope(envelope).await?;
        Ok(true)
    }
}

/// Standard header consumer-adapter dispatch keys on. Continuum,
/// hermes, openclaw etc. set this on publish; the registry's
/// [`AdapterRegistry::dispatch`] reads it to find the right adapter.
pub const HEADER_AIRC_BODY_HINT: &str = "airc.body_hint";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory adapter that records every envelope it receives.
    /// Used as the test foundation; future consumer adapters in
    /// continuum/hermes/openclaw follow the same pattern.
    struct RecordingAdapter {
        name: &'static str,
        body_hint: &'static str,
        received: Mutex<Vec<TranscriptEvent>>,
    }

    impl RecordingAdapter {
        fn new(name: &'static str, body_hint: &'static str) -> Self {
            Self {
                name,
                body_hint,
                received: Mutex::new(Vec::new()),
            }
        }

        fn received_count(&self) -> usize {
            self.received.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl ConsumerAdapter for RecordingAdapter {
        fn name(&self) -> &'static str {
            self.name
        }
        fn body_hint(&self) -> &'static str {
            self.body_hint
        }
        async fn on_envelope(&self, envelope: TranscriptEvent) -> Result<(), AdapterError> {
            self.received.lock().unwrap().push(envelope);
            Ok(())
        }
    }

    fn envelope_with_body_hint(hint: &str) -> TranscriptEvent {
        use airc_core::headers::Headers;
        use airc_core::transcript::MentionTarget;
        use airc_core::{ClientId, EventId, PeerId, RoomId, TranscriptKind};
        let mut headers = Headers::new();
        headers.insert(HEADER_AIRC_BODY_HINT.to_string(), hint.to_string());
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::from_u128(0xcafe),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000,
            lamport: 1,
            target: MentionTarget::All,
            headers,
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn register_and_dispatch_routes_to_matching_adapter() {
        let registry = AdapterRegistry::new();
        let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
        registry.register(continuum.clone()).unwrap();

        let env = envelope_with_body_hint("forge.persona.event.v1");
        let dispatched = registry.dispatch(env).await.unwrap();

        assert!(dispatched);
        assert_eq!(continuum.received_count(), 1);
    }

    #[tokio::test]
    async fn dispatch_returns_false_when_no_adapter_claims_body_hint() {
        let registry = AdapterRegistry::new();
        let env = envelope_with_body_hint("forge.unknown.v1");

        let dispatched = registry.dispatch(env).await.unwrap();
        assert!(!dispatched);
    }

    #[tokio::test]
    async fn dispatch_returns_false_when_envelope_has_no_body_hint_header() {
        let registry = AdapterRegistry::new();
        let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
        registry.register(continuum.clone()).unwrap();

        use airc_core::headers::Headers;
        use airc_core::transcript::MentionTarget;
        use airc_core::{ClientId, EventId, PeerId, RoomId, TranscriptKind};
        let env = TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::from_u128(0xcafe),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000,
            lamport: 1,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };

        assert!(!registry.dispatch(env).await.unwrap());
        assert_eq!(continuum.received_count(), 0);
    }

    #[tokio::test]
    async fn multi_adapter_coexistence_routes_by_body_hint() {
        let registry = AdapterRegistry::new();
        let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
        let hermes = Arc::new(RecordingAdapter::new("hermes", "forge.hermes.plan.v1"));
        registry.register(continuum.clone()).unwrap();
        registry.register(hermes.clone()).unwrap();

        let persona_env = envelope_with_body_hint("forge.persona.event.v1");
        let plan_env = envelope_with_body_hint("forge.hermes.plan.v1");

        registry.dispatch(persona_env).await.unwrap();
        registry.dispatch(plan_env).await.unwrap();

        assert_eq!(continuum.received_count(), 1);
        assert_eq!(hermes.received_count(), 1);
    }

    #[test]
    fn duplicate_name_is_rejected() {
        let registry = AdapterRegistry::new();
        let a = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
        let b = Arc::new(RecordingAdapter::new("continuum", "different.hint.v1"));
        registry.register(a).unwrap();
        let err = registry.register(b).unwrap_err();
        assert!(matches!(err, RegistryError::DuplicateName("continuum")));
    }

    #[test]
    fn duplicate_body_hint_is_rejected_with_actionable_message() {
        let registry = AdapterRegistry::new();
        let first = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
        // Hermes accidentally claims continuum's hint — registry
        // refuses, surfaces both names so operator can fix.
        let dup = Arc::new(RecordingAdapter::new("hermes", "forge.persona.event.v1"));
        registry.register(first).unwrap();
        let err = registry.register(dup).unwrap_err();
        match err {
            RegistryError::DuplicateBodyHint {
                existing_name,
                new_name,
                body_hint,
            } => {
                assert_eq!(existing_name, "continuum");
                assert_eq!(new_name, "hermes");
                assert_eq!(body_hint, "forge.persona.event.v1");
            }
            other => panic!("expected DuplicateBodyHint, got {other:?}"),
        }
    }

    #[test]
    fn deregister_removes_adapter_so_name_and_hint_become_reusable() {
        let registry = AdapterRegistry::new();
        let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
        registry.register(continuum.clone()).unwrap();

        let removed = registry.deregister("continuum").unwrap();
        assert!(removed.is_some());
        assert_eq!(registry.registered_names(), Vec::<&'static str>::new());

        // Re-register: name + hint are reusable now.
        let fresh = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
        registry.register(fresh).unwrap();
    }

    #[test]
    fn deregister_unknown_adapter_returns_none() {
        let registry = AdapterRegistry::new();
        let removed = registry.deregister("ghost").unwrap();
        assert!(removed.is_none());
    }

    #[test]
    fn registered_names_returns_alphabetical_snapshot() {
        let registry = AdapterRegistry::new();
        registry
            .register(Arc::new(RecordingAdapter::new(
                "openclaw",
                "forge.openclaw.tool.v1",
            )))
            .unwrap();
        registry
            .register(Arc::new(RecordingAdapter::new(
                "continuum",
                "forge.persona.event.v1",
            )))
            .unwrap();
        registry
            .register(Arc::new(RecordingAdapter::new(
                "hermes",
                "forge.hermes.plan.v1",
            )))
            .unwrap();

        assert_eq!(
            registry.registered_names(),
            vec!["continuum", "hermes", "openclaw"],
        );
    }
}
