//! Integration: AIRC-event diagnostic sink round-trip.
//!
//! Proves work card 524c7727: a substrate emitter publishes a
//! `DiagnosticEvent` via the AIRC-event sink; a separate Airc
//! instance subscribed over a shared wire receives it as a typed
//! `DiagnosticEvent`. No stdout parsing.

use std::time::Duration;

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSeverity, DiagnosticSink,
};
use airc_lib::{Airc, AircEventDiagnosticSink, PeerSpec};
use futures::stream::StreamExt;
use tempfile::TempDir;

#[tokio::test]
async fn diagnostic_event_sink_publishes_to_airc_subscribers() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice.add_peer(bob_spec).await.expect("trust");
    bob.add_peer(alice_spec).await.expect("trust");

    alice
        .join_with_wire("diag-sink-test", wire_path.clone())
        .await
        .expect("alice joins");
    bob.join_with_wire("diag-sink-test", wire_path)
        .await
        .expect("bob joins");

    // Bob subscribes to typed diagnostic events BEFORE alice emits.
    let mut stream = Box::pin(
        bob.subscribe_diagnostic_events()
            .await
            .expect("subscribe diagnostics"),
    );

    // Tiny settle so bob's subscriber attaches.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Alice constructs the AIRC-event sink and emits a real
    // diagnostic through it. This is the substrate's intended use:
    // any code that holds a `DiagnosticSink` can emit and observers
    // receive typed events.
    let sink = AircEventDiagnosticSink::new(alice.clone());
    let diag = DiagnosticEvent::warn(
        DiagnosticComponent::Transport,
        DiagnosticCode::FrameVerificationFailed,
        "integration-test diagnostic",
    )
    .with_field("scope", "diag-sink-test");
    sink.emit(diag);

    // Pull until our diagnostic arrives.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut got = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some((_transcript_event, diag))) => {
                if diag.message == "integration-test diagnostic" {
                    got = Some(diag);
                    break;
                }
            }
            Ok(None) => panic!("subscription closed before our event"),
            Err(_) => continue,
        }
    }

    let diag = got.expect("diagnostic should arrive at subscriber");
    assert_eq!(diag.severity, DiagnosticSeverity::Warn);
    assert_eq!(diag.component, DiagnosticComponent::Transport);
    assert_eq!(diag.code, DiagnosticCode::FrameVerificationFailed);
    assert_eq!(diag.message, "integration-test diagnostic");
    assert_eq!(
        diag.fields.get("scope").map(String::as_str),
        Some("diag-sink-test")
    );
}

#[tokio::test]
async fn recent_diagnostic_events_reads_back_from_transcript() {
    let home = TempDir::new().expect("tempdir");
    let wire_dir = TempDir::new().expect("wire");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let airc = Airc::open(home.path()).await.expect("open");
    airc.join_with_wire("recent-diag-test", wire_path)
        .await
        .expect("join");

    let sink = AircEventDiagnosticSink::new(airc.clone());
    let diag = DiagnosticEvent::error(
        DiagnosticComponent::WebRtc,
        DiagnosticCode::WebRtcOfferAnswerFailed,
        "test recent error",
    );
    sink.emit(diag);

    tokio::time::sleep(Duration::from_millis(100)).await;

    let recent = airc
        .recent_diagnostic_events(64)
        .await
        .expect("recent query");
    assert!(
        recent
            .iter()
            .any(|d| d.message == "test recent error" && d.severity == DiagnosticSeverity::Error),
        "recent_diagnostic_events should include our emitted error; got {recent:?}"
    );
}
