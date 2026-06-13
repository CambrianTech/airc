//! Integration: AIRC-event diagnostic sink round-trip, over the daemon.
//!
//! Proves work card 524c7727: a substrate emitter publishes a
//! `DiagnosticEvent` via the AIRC-event sink; a separate scope on the
//! same machine — attached to the one owner-core daemon — receives it
//! as a typed `DiagnosticEvent`, and `recent_diagnostic_events` reads
//! it back from the daemon's durable transcript. No stdout parsing,
//! no file wire.

mod common;

use std::time::Duration;

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSeverity, DiagnosticSink,
};
use airc_lib::AircEventDiagnosticSink;
use common::Machine;
use futures::stream::StreamExt;

#[tokio::test]
async fn diagnostic_event_sink_publishes_to_daemon_subscribers() {
    let machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("diag-sink-test").await;

    // Bob subscribes to typed diagnostic events BEFORE alice emits.
    let mut stream = Box::pin(
        bob.subscribe_diagnostic_events()
            .await
            .expect("subscribe diagnostics"),
    );

    // Tiny settle so bob's daemon attach is live.
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
async fn recent_diagnostic_events_reads_back_from_daemon_transcript() {
    let machine = Machine::boot().await;
    let airc = machine.solo("recent-diag-test").await;

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
