//! Integration: tracing macro → AIRC tracing-subscriber layer →
//! background drain task → published Event frame → typed subscriber
//! sees `DiagnosticEvent`.
//!
//! Proves the end-to-end contract for work card 49ba8abf: a library
//! emitter calls `tracing::error!("...")` and an integration
//! subscribed to AIRC events receives a typed
//! `DiagnosticEvent` body, no stdout/stderr parsing required.

use std::time::Duration;

use airc_lib::{Airc, DiagnosticLevel};
use futures::stream::StreamExt;
use tempfile::TempDir;

#[tokio::test]
async fn tracing_macros_publish_typed_diagnostic_events() {
    let home = TempDir::new().expect("tempdir");
    let airc = Airc::open(home.path()).await.expect("open");
    let _ = airc.join("diagnostic-pipeline-test").await.expect("join");

    // Install the AIRC tracing subscriber. This registers the
    // global tracing-subscriber and spawns the drain task that
    // publishes diagnostics as Event frames.
    airc.install_diagnostic_subscriber()
        .await
        .expect("install subscriber");

    // Subscribe to the typed diagnostic stream BEFORE emitting so
    // we don't race the drain. `Box::pin` to give the filter-map
    // stream a stable address; the returned `impl Stream` is not
    // `Unpin` because its filter_map closures aren't.
    let mut stream = Box::pin(airc.subscribe_diagnostics().await.expect("subscribe"));

    // Emit via the tracing macro from this test's "library" context.
    // The macro fires synchronously; the layer enqueues the
    // diagnostic to the drain task; the drain task publishes.
    tracing::warn!("integration-test diagnostic message");

    // Pull until we see our diagnostic.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut got = None;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some((_event, diag))) => {
                if diag.message == "integration-test diagnostic message" {
                    got = Some(diag);
                    break;
                }
            }
            Ok(None) => panic!("diagnostic stream closed before our event"),
            Err(_) => continue,
        }
    }

    let diag = got.expect("integration-test diagnostic should reach the subscriber");
    assert_eq!(diag.level, DiagnosticLevel::Warn);
    assert_eq!(diag.message, "integration-test diagnostic message");
    assert!(
        !diag.target.is_empty(),
        "diagnostic target (module path) should be set; got empty"
    );
}
