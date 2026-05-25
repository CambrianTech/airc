//! AIRC-event publication sink for `DiagnosticEvent`s.
//!
//! Closes work card 524c7727 (P1, "Diagnostic sink follow-up:
//! ORM/event sink and doctor surface"). #973 shipped the
//! `DiagnosticSink` trait + `MemoryDiagnosticSink` /
//! `StderrJsonDiagnosticSink`. This PR adds the AIRC-event variant:
//! a sink that publishes each diagnostic as an AIRC Event frame so
//! integrations on any subscribed peer can decode and act on
//! diagnostics without parsing stdout/stderr.
//!
//! Pairs with a typed subscribe + query API on `Airc` so dashboards,
//! monitor, and `airc doctor` can read recent diagnostics from the
//! transcript without screen-scraping any text surface.
//!
//! Scope per card: ORM-backed projection is the "or" branch;
//! AIRC-event publication is the "and" branch this PR takes. The
//! ORM projection is a perf/durability optimization that can land as
//! a follow-up once a callsite needs it.

use std::sync::Arc;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, TranscriptEvent};
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSeverity, DiagnosticSink,
};
use airc_protocol::FrameKind;
use futures::stream::{Stream, StreamExt};
use tokio::runtime::Handle;

use crate::error::AircError;
use crate::Airc;

/// Header keys for filtering diagnostics on the wire without
/// decoding the body. Stable string values (snake_case enum variant
/// names) so dashboards can match against them directly.
pub const HEADER_DIAG_SEVERITY: &str = "airc.diag.severity";
pub const HEADER_DIAG_COMPONENT: &str = "airc.diag.component";
pub const HEADER_DIAG_CODE: &str = "airc.diag.code";

/// `DiagnosticSink` impl that publishes each event as an AIRC
/// `FrameKind::Event` frame, tagged with severity/component/code
/// headers and a JSON-encoded `DiagnosticEvent` body.
///
/// The `emit()` callback fires synchronously from any thread; the
/// publish itself is async, so the sink spawns a tokio task per
/// event. Best-effort: if no tokio runtime is in scope (callsite
/// running on a foreign thread), the event is dropped. Diagnostics
/// must never panic or block the emitter.
#[derive(Clone)]
pub struct AircEventDiagnosticSink {
    airc: Airc,
}

impl AircEventDiagnosticSink {
    pub fn new(airc: Airc) -> Self {
        Self { airc }
    }
}

impl DiagnosticSink for AircEventDiagnosticSink {
    fn emit(&self, event: DiagnosticEvent) {
        let airc = self.airc.clone();
        // Sinks can be called from any thread / context. If a tokio
        // runtime is available, spawn the publish task. If not, the
        // event drops silently — diagnostics must never block or
        // panic the caller.
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = airc.publish_diagnostic_event(&event).await {
                    // Fall back to stderr only here — this is the
                    // one allowed eprintln: the publish path itself
                    // failed, and the sink can't recursively emit
                    // through itself.
                    eprintln!("airc diagnostic publish failed: {error}");
                }
            });
        }
    }
}

impl Airc {
    /// Publish a single `DiagnosticEvent` as an AIRC Event frame
    /// with stable severity/component/code headers. Normally called
    /// from `AircEventDiagnosticSink::emit`; consumers can also call
    /// this directly when they have a pre-built event.
    pub async fn publish_diagnostic_event(&self, event: &DiagnosticEvent) -> Result<(), AircError> {
        let body = serde_json::to_value(event)
            .map_err(|error| AircError::Crypto(format!("diagnostic event encode: {error}")))?;
        let mut headers = Headers::new();
        headers.insert(
            HEADER_DIAG_SEVERITY.into(),
            severity_header_value(event.severity).to_string(),
        );
        headers.insert(
            HEADER_DIAG_COMPONENT.into(),
            component_header_value(event.component).to_string(),
        );
        headers.insert(
            HEADER_DIAG_CODE.into(),
            code_header_value(event.code).to_string(),
        );
        self.send_frame_to(
            FrameKind::Event,
            MentionTarget::All,
            Body::Json(body),
            headers,
        )
        .await?;
        Ok(())
    }

    /// Live stream of typed `DiagnosticEvent`s observed on the
    /// substrate. Filters the raw transcript subscription to events
    /// carrying the `airc.diag.severity` header and decodes their
    /// body.
    pub async fn subscribe_diagnostic_events(
        &self,
    ) -> Result<impl Stream<Item = (Arc<TranscriptEvent>, DiagnosticEvent)>, AircError> {
        let inner = self.subscribe().await?;
        Ok(inner.filter_map(|item| async move {
            let event = item.ok()?;
            let diag = parse_diagnostic_event(&event)?;
            Some((event, diag))
        }))
    }

    /// Query recent diagnostics from the persisted transcript.
    /// Walks the last `window` events and returns matching
    /// `DiagnosticEvent`s in transcript order (oldest → newest).
    /// Useful for `airc doctor` surfacing recent errors without
    /// holding a live subscription.
    pub async fn recent_diagnostic_events(
        &self,
        window: usize,
    ) -> Result<Vec<DiagnosticEvent>, AircError> {
        let recent = self.page_recent(window).await?;
        let mut out = Vec::with_capacity(recent.len().min(window));
        for transcript_event in recent {
            if let Some(diag) = parse_diagnostic_event(&transcript_event) {
                out.push(diag);
            }
        }
        Ok(out)
    }
}

fn parse_diagnostic_event(event: &TranscriptEvent) -> Option<DiagnosticEvent> {
    let _ = event.headers.get(HEADER_DIAG_SEVERITY)?;
    let body = event.body.as_ref()?;
    let value = match body {
        Body::Json(value) => value.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

fn severity_header_value(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Debug => "debug",
        DiagnosticSeverity::Info => "info",
        DiagnosticSeverity::Warn => "warn",
        DiagnosticSeverity::Error => "error",
    }
}

fn component_header_value(component: DiagnosticComponent) -> &'static str {
    match component {
        DiagnosticComponent::Daemon => "daemon",
        DiagnosticComponent::Monitor => "monitor",
        DiagnosticComponent::Replay => "replay",
        DiagnosticComponent::Subscriber => "subscriber",
        DiagnosticComponent::Transport => "transport",
        DiagnosticComponent::WebRtc => "webrtc",
        DiagnosticComponent::Work => "work",
    }
}

fn code_header_value(code: DiagnosticCode) -> &'static str {
    match code {
        DiagnosticCode::ConnectionError => "connection_error",
        DiagnosticCode::FrameVerificationFailed => "frame_verification_failed",
        DiagnosticCode::StoreAppendFailed => "store_append_failed",
        DiagnosticCode::TrustRefreshFailed => "trust_refresh_failed",
        DiagnosticCode::WireLostEmitFailed => "wire_lost_emit_failed",
        DiagnosticCode::MalformedReplayFrameSkipped => "malformed_replay_frame_skipped",
        DiagnosticCode::UnverifiableReplayFrameSkipped => "unverifiable_replay_frame_skipped",
        DiagnosticCode::ReplayFramesSkipped => "replay_frames_skipped",
        DiagnosticCode::WebRtcOfferAnswerFailed => "webrtc_offer_answer_failed",
        DiagnosticCode::WorkspaceLeaseViolation => "workspace_lease_violation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_headers_stable() {
        assert_eq!(severity_header_value(DiagnosticSeverity::Debug), "debug");
        assert_eq!(severity_header_value(DiagnosticSeverity::Info), "info");
        assert_eq!(severity_header_value(DiagnosticSeverity::Warn), "warn");
        assert_eq!(severity_header_value(DiagnosticSeverity::Error), "error");
    }

    #[test]
    fn component_headers_stable() {
        for component in [
            DiagnosticComponent::Daemon,
            DiagnosticComponent::Monitor,
            DiagnosticComponent::Replay,
            DiagnosticComponent::Subscriber,
            DiagnosticComponent::Transport,
            DiagnosticComponent::WebRtc,
            DiagnosticComponent::Work,
        ] {
            let header = component_header_value(component);
            assert!(!header.is_empty(), "{component:?} should map to non-empty");
        }
    }

    #[test]
    fn code_headers_stable() {
        for code in [
            DiagnosticCode::ConnectionError,
            DiagnosticCode::FrameVerificationFailed,
            DiagnosticCode::StoreAppendFailed,
            DiagnosticCode::TrustRefreshFailed,
            DiagnosticCode::WireLostEmitFailed,
            DiagnosticCode::MalformedReplayFrameSkipped,
            DiagnosticCode::UnverifiableReplayFrameSkipped,
            DiagnosticCode::ReplayFramesSkipped,
            DiagnosticCode::WebRtcOfferAnswerFailed,
            DiagnosticCode::WorkspaceLeaseViolation,
        ] {
            let header = code_header_value(code);
            assert!(!header.is_empty(), "{code:?} should map to non-empty");
        }
    }
}
