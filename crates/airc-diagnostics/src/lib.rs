//! Typed operational diagnostics for AIRC.
//!
//! Diagnostics are not protocol output. Substrate code emits
//! [`DiagnosticEvent`] values through a [`DiagnosticSink`]; terminal
//! hosts can render them to stderr, and integrations can route them to
//! ORM/event projections without parsing CLI text.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticComponent {
    Daemon,
    Monitor,
    Replay,
    Subscriber,
    Transport,
    WebRtc,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    ConnectionError,
    FrameVerificationFailed,
    StoreAppendFailed,
    TrustRefreshFailed,
    WireLostEmitFailed,
    MalformedReplayFrameSkipped,
    UnverifiableReplayFrameSkipped,
    ReplayFramesSkipped,
    WebRtcOfferAnswerFailed,
    WorkspaceLeaseViolation,
    /// Card 625abe6d slice 2: the daemon's periodic route-discovery
    /// refresh failed as a whole (substrate handle could not open, or
    /// the refresh itself errored). The loop retries next interval.
    RouteRefreshFailed,
    /// Card 625abe6d slice 2: one stored peer endpoint did not answer
    /// an outbound route-discovery dial. Offline peers are normal mesh
    /// weather — but every failed dial attempt must be visible.
    PeerDialFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticEvent {
    pub severity: DiagnosticSeverity,
    pub component: DiagnosticComponent,
    pub code: DiagnosticCode,
    pub message: String,
    pub fields: BTreeMap<String, String>,
    pub occurred_at_ms: u64,
}

impl DiagnosticEvent {
    pub fn new(
        severity: DiagnosticSeverity,
        component: DiagnosticComponent,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            component,
            code,
            message: message.into(),
            fields: BTreeMap::new(),
            occurred_at_ms: now_ms(),
        }
    }

    pub fn warn(
        component: DiagnosticComponent,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> Self {
        Self::new(DiagnosticSeverity::Warn, component, code, message)
    }

    pub fn error(
        component: DiagnosticComponent,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> Self {
        Self::new(DiagnosticSeverity::Error, component, code, message)
    }

    pub fn with_field(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.fields.insert(key.into(), value.to_string());
        self
    }
}

pub trait DiagnosticSink: Send + Sync {
    fn emit(&self, event: DiagnosticEvent);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopDiagnosticSink;

impl DiagnosticSink for NoopDiagnosticSink {
    fn emit(&self, _event: DiagnosticEvent) {}
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StderrJsonDiagnosticSink;

impl DiagnosticSink for StderrJsonDiagnosticSink {
    fn emit(&self, event: DiagnosticEvent) {
        let mut stderr = std::io::stderr().lock();
        let _ = serde_json::to_writer(&mut stderr, &event);
        let _ = stderr.write_all(b"\n");
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemoryDiagnosticSink {
    events: Arc<Mutex<Vec<DiagnosticEvent>>>,
}

impl MemoryDiagnosticSink {
    pub fn events(&self) -> Vec<DiagnosticEvent> {
        match self.events.lock() {
            Ok(events) => events.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl DiagnosticSink for MemoryDiagnosticSink {
    fn emit(&self, event: DiagnosticEvent) {
        match self.events.lock() {
            Ok(mut events) => events.push(event),
            Err(poisoned) => poisoned.into_inner().push(event),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSeverity, DiagnosticSink,
        MemoryDiagnosticSink,
    };

    #[test]
    fn memory_sink_captures_typed_events() {
        let sink = MemoryDiagnosticSink::default();
        sink.emit(
            DiagnosticEvent::warn(
                DiagnosticComponent::Replay,
                DiagnosticCode::UnverifiableReplayFrameSkipped,
                "skipped unverifiable frame",
            )
            .with_field("peer_id", "peer-1"),
        );

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].severity, DiagnosticSeverity::Warn);
        assert_eq!(events[0].component, DiagnosticComponent::Replay);
        assert_eq!(
            events[0].code,
            DiagnosticCode::UnverifiableReplayFrameSkipped
        );
        assert_eq!(events[0].fields.get("peer_id"), Some(&"peer-1".to_string()));
    }

    #[test]
    fn event_serializes_as_stable_snake_case_json() {
        let event = DiagnosticEvent::error(
            DiagnosticComponent::Daemon,
            DiagnosticCode::ConnectionError,
            "connection failed",
        )
        .with_field("socket", "daemon.sock");

        let value = serde_json::to_value(event).expect("diagnostic json");
        assert_eq!(value["severity"], "error");
        assert_eq!(value["component"], "daemon");
        assert_eq!(value["code"], "connection_error");
        assert_eq!(value["fields"]["socket"], "daemon.sock");
    }
}
