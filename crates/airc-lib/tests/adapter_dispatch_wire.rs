//! Card df92581a Sub-2 — integration test that an adapter registered via
//! `Airc::register_adapter` actually receives envelopes that pass through
//! the Airc instance's live stream.
//!
//! Written FIRST per TDD/VDD discipline before the wire implementation
//! lands. Pins the substrate contract continuum-core depends on:
//!
//!   1. `Airc::register_adapter(Arc<dyn ConsumerAdapter>)` makes the
//!      adapter visible to the dispatch loop.
//!   2. An inbound envelope whose `airc.body_hint` matches a registered
//!      adapter's `body_hint()` lands at that adapter's `on_envelope`.
//!   3. An envelope with NO matching adapter is silently dropped (no
//!      panic, no error escalation, the substrate just doesn't have
//!      anyone to deliver to).
//!   4. Adapter `on_envelope` errors are non-fatal — the next envelope
//!      still gets dispatched.
//!
//! Failure mode the suite catches: the dispatch wire being missing
//! entirely (continuum-core registers an adapter, sends an envelope,
//! and the adapter's recorded counter stays at zero). That's the
//! exact failure mode the continuum agent flagged as blocking.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{ClientId, EventId, PeerId, RoomId, TranscriptKind};
use airc_lib::adapter::{AdapterError, ConsumerAdapter, RegistryError, HEADER_AIRC_BODY_HINT};
use airc_lib::{Airc, TranscriptEvent};
use tempfile::TempDir;

/// Same shape as `adapter.rs`'s `RecordingAdapter` test fixture, but
/// usable from the integration-test crate (the unit-test version is
/// private to the module).
struct RecordingAdapter {
    name: &'static str,
    body_hint: &'static str,
    received: Mutex<Vec<TranscriptEvent>>,
    fail_with: Mutex<Option<&'static str>>,
}

impl RecordingAdapter {
    fn new(name: &'static str, body_hint: &'static str) -> Self {
        Self {
            name,
            body_hint,
            received: Mutex::new(Vec::new()),
            fail_with: Mutex::new(None),
        }
    }

    fn received_count(&self) -> usize {
        self.received.lock().unwrap().len()
    }

    fn arm_failure(&self, msg: &'static str) {
        *self.fail_with.lock().unwrap() = Some(msg);
    }

    fn disarm_failure(&self) {
        *self.fail_with.lock().unwrap() = None;
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
        // Record FIRST so an arm_failure() still counts the receive
        // — that's how the substrate sees a delivery attempt.
        self.received.lock().unwrap().push(envelope);
        if let Some(msg) = *self.fail_with.lock().unwrap() {
            return Err(AdapterError::Consumer(msg.to_string()));
        }
        Ok(())
    }
}

fn envelope_with_body_hint(hint: &str, lamport: u64) -> TranscriptEvent {
    let mut headers = Headers::new();
    headers.insert(HEADER_AIRC_BODY_HINT.to_string(), hint.to_string());
    TranscriptEvent {
        event_id: EventId::new(),
        room_id: RoomId::from_u128(0xcafe),
        peer_id: PeerId::new(),
        client_id: ClientId::new(),
        kind: TranscriptKind::Message,
        occurred_at_ms: 1_700_000_000_000,
        lamport,
        target: MentionTarget::All,
        headers,
        body: None,
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}

/// Push an envelope into the Airc's live broadcast stream. Mirrors the
/// pattern existing call sites (transport.rs, lifecycle.rs,
/// messaging.rs) use to feed events into live_tx.
async fn emit_into_live_stream(airc: &Airc, envelope: TranscriptEvent) {
    airc.emit_for_dispatch_test(Arc::new(envelope))
        .await
        .expect("test helper must succeed");
}

/// Wait briefly for the dispatch task to consume from live_tx. The
/// dispatch task runs concurrently with the test; polling beats a
/// fixed sleep because most assertions land within a few ms but
/// scheduling fuzz can push them to ~100ms on a loaded CI runner.
async fn wait_for_received(adapter: &Arc<RecordingAdapter>, expected: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if adapter.received_count() >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "timed out waiting for adapter {:?} to receive {} envelopes (last count = {})",
        adapter.name,
        expected,
        adapter.received_count()
    );
}

#[tokio::test]
async fn register_adapter_makes_registry_observable() {
    // V1: after register_adapter, the adapter's name is in
    // adapter_registry().registered_names(). Catches a wire that
    // accepts the registration but stores it in a registry the
    // dispatch loop doesn't read.
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join("home"))
        .await
        .expect("open airc");

    let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
    airc.register_adapter(continuum.clone())
        .expect("register succeeds");

    let names = airc.adapter_registry().registered_names();
    assert_eq!(names, vec!["continuum"]);
}

#[tokio::test]
async fn matching_envelope_lands_at_registered_adapter() {
    // V2 (the critical wire): an envelope whose body_hint matches a
    // registered adapter's body_hint reaches the adapter's
    // on_envelope. THIS IS THE TEST that was previously zero-passing
    // because the wire didn't exist.
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join("home"))
        .await
        .expect("open airc");

    let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
    airc.register_adapter(continuum.clone())
        .expect("register succeeds");

    emit_into_live_stream(&airc, envelope_with_body_hint("forge.persona.event.v1", 1)).await;

    wait_for_received(&continuum, 1).await;
    assert_eq!(continuum.received_count(), 1);
}

#[tokio::test]
async fn envelope_without_matching_adapter_is_silently_dropped() {
    // V3: no adapter claims the body_hint → no panic, no error
    // bubbling up, the dispatch task just keeps running. Validates
    // the dispatch loop's failure-tolerance.
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join("home"))
        .await
        .expect("open airc");

    let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
    airc.register_adapter(continuum.clone())
        .expect("register succeeds");

    // Emit a hint no adapter claims.
    emit_into_live_stream(&airc, envelope_with_body_hint("hermes.skill.event.v1", 1)).await;
    // Then emit one that DOES match — proves the dispatch loop kept
    // running past the no-match case.
    emit_into_live_stream(&airc, envelope_with_body_hint("forge.persona.event.v1", 2)).await;

    wait_for_received(&continuum, 1).await;
    assert_eq!(
        continuum.received_count(),
        1,
        "only the matching envelope should reach continuum"
    );
}

#[tokio::test]
async fn adapter_error_does_not_kill_dispatch_loop() {
    // V4: when an adapter's on_envelope returns Err, the dispatch
    // task logs and continues. Pins the failure-tolerance
    // substrate property continuum-core relies on (one bad consumer
    // mustn't take down the substrate for the others).
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join("home"))
        .await
        .expect("open airc");

    let continuum = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
    airc.register_adapter(continuum.clone())
        .expect("register succeeds");

    continuum.arm_failure("synthetic translation error");
    emit_into_live_stream(&airc, envelope_with_body_hint("forge.persona.event.v1", 1)).await;
    wait_for_received(&continuum, 1).await;
    continuum.disarm_failure();

    // Now emit a second envelope — if the dispatch task died after
    // the error, this never gets recorded.
    emit_into_live_stream(&airc, envelope_with_body_hint("forge.persona.event.v1", 2)).await;
    wait_for_received(&continuum, 2).await;
    assert_eq!(continuum.received_count(), 2);
}

#[tokio::test]
async fn register_adapter_rejects_duplicate_name() {
    // V5: the registry's invariants surface through Airc's surface
    // intact — a re-register without deregister returns the typed
    // RegistryError::DuplicateName, not a silent overwrite (which
    // would create dispatch ambiguity continuum-core can't
    // recover from).
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join("home"))
        .await
        .expect("open airc");

    let a = Arc::new(RecordingAdapter::new("continuum", "forge.persona.event.v1"));
    let b = Arc::new(RecordingAdapter::new("continuum", "forge.other.event.v1"));
    airc.register_adapter(a).expect("first register");

    let err = airc
        .register_adapter(b)
        .expect_err("duplicate name must refuse");
    assert!(
        matches!(err, RegistryError::DuplicateName("continuum")),
        "expected DuplicateName, got {err:?}"
    );
}
