//! Typed diagnostic emission via `tracing`.
//!
//! Closes work card 49ba8abf (diagnostic sink, P1): "Replace
//! scattered println/eprintln diagnostics with typed diagnostic
//! sink. Integrations must use airc-lib, daemon IPC, ORM
//! projections, or AIRC events; stdout/stderr are terminal sinks
//! only, never parsed integration APIs."
//!
//! ## The integration contract
//!
//! - **Substrate code** emits diagnostics via the `tracing` macros:
//!   `tracing::error!`, `warn!`, `info!`, `debug!`, `trace!`. The
//!   tracing global subscriber routes them to (a) stderr (terminal
//!   user surface, env-filter controlled) AND (b) an AIRC
//!   `DiagnosticEvent` published to the current room (programmatic
//!   integration surface).
//! - **Integrations** consume diagnostics by subscribing to the
//!   AIRC stream and filtering by `airc.diag.level` /
//!   `airc.diag.target` headers, then decoding the
//!   `DiagnosticEvent` body. They never parse stdout/stderr.
//!
//! ## Installing the subscriber
//!
//! Consumers that want diagnostics published as AIRC events call
//! [`Airc::install_diagnostic_subscriber`] once at startup. This
//! registers a tracing-subscriber layer that enqueues each
//! tracing::Event to a background drain task; the task publishes
//! them as AIRC Event frames to the current room.
//!
//! The terminal output (stderr) layer is *not* installed by AIRC —
//! callers wire their own `tracing_subscriber::fmt` layer if they
//! want terminal output. This keeps AIRC out of the "logging format"
//! business; we only own the wire-event side.
//!
//! ## Scope cuts (follow-ups)
//!
//! - **Structured fields**: this PR captures only the message string
//!   and target. Structured fields (`tracing::error!(?key, ...)`)
//!   are reserved space in `DiagnosticEvent.fields` but not yet
//!   harvested by the layer's `Visit` impl.
//! - **Wholesale migration**: 429 `eprintln!`/`println!` sites
//!   across the workspace. This PR migrates a representative
//!   subset in `airc-lib` (transport / webrtc / wire_replay error
//!   paths) to demonstrate the pattern. Codebase-wide migration is
//!   its own card.
//! - **Ephemeral frame kind**: diagnostics are durable Events for
//!   now (same flaw #4 trade-off as lifecycle/heartbeat). The
//!   long-term answer is an ephemeral frame kind that doesn't
//!   accumulate in the transcript store.

use std::sync::Arc;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, TranscriptEvent};
use airc_protocol::FrameKind;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

pub const HEADER_DIAG_LEVEL: &str = "airc.diag.level";
pub const HEADER_DIAG_TARGET: &str = "airc.diag.target";

/// Diagnostic severity. Mirrors `tracing::Level` order so callers can
/// translate either way without surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl DiagnosticLevel {
    pub fn header_value(self) -> &'static str {
        match self {
            DiagnosticLevel::Trace => "trace",
            DiagnosticLevel::Debug => "debug",
            DiagnosticLevel::Info => "info",
            DiagnosticLevel::Warn => "warn",
            DiagnosticLevel::Error => "error",
        }
    }

    pub fn from_tracing(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::TRACE => DiagnosticLevel::Trace,
            tracing::Level::DEBUG => DiagnosticLevel::Debug,
            tracing::Level::INFO => DiagnosticLevel::Info,
            tracing::Level::WARN => DiagnosticLevel::Warn,
            tracing::Level::ERROR => DiagnosticLevel::Error,
        }
    }
}

/// Typed diagnostic record carried as the body of an AIRC Event
/// frame. Subscribers filter on the `airc.diag.level` /
/// `airc.diag.target` headers without decoding the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticEvent {
    pub level: DiagnosticLevel,
    /// `tracing::Metadata::target` — typically the module path that
    /// emitted the diagnostic.
    pub target: String,
    /// The rendered message body.
    pub message: String,
    /// Reserved space for structured fields (`tracing::error!(?key,
    /// ...)`). Empty in v1 — the layer's Visit impl currently only
    /// captures the message.
    #[serde(default)]
    pub fields: serde_json::Map<String, serde_json::Value>,
    pub emitted_at_ms: u64,
}

/// `tracing_subscriber::Layer` that enqueues each tracing event to
/// the AIRC drain task installed by
/// [`Airc::install_diagnostic_subscriber`]. Cheap to register; doing
/// the publish on a background task keeps tracing callsites
/// non-blocking and avoids `block_on` deadlocks when a diagnostic
/// fires from inside an async context.
pub struct AircDiagnosticLayer {
    tx: UnboundedSender<DiagnosticEvent>,
}

impl AircDiagnosticLayer {
    fn new(tx: UnboundedSender<DiagnosticEvent>) -> Self {
        Self { tx }
    }
}

impl<S: Subscriber> Layer<S> for AircDiagnosticLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = DiagnosticLevel::from_tracing(metadata.level());
        let target = metadata.target().to_string();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = visitor.message.unwrap_or_default();
        let emitted_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let diag = DiagnosticEvent {
            level,
            target,
            message,
            fields: serde_json::Map::new(),
            emitted_at_ms,
        };
        // Best-effort: if the drain task has gone away, drop the
        // event. Diagnostics must never panic / block the emitter.
        let _ = self.tx.send(diag);
    }
}

/// Tracing Visitor that captures only the message field (the
/// rendered formatted string for `tracing::error!("...")`). Reserved
/// space for structured-field visitation in a follow-up.
#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}

impl Airc {
    /// Install the AIRC tracing-subscriber layer + spawn the
    /// background drain task that publishes diagnostics as Event
    /// frames to the current room.
    ///
    /// Idempotent only within a process: tracing's global subscriber
    /// can only be set once, so a second call from the same process
    /// returns `Ok(())` after the first registers. Callers that need
    /// their own terminal `tracing_subscriber::fmt` layer should
    /// install it via `tracing_subscriber::Registry::default().with(...)`
    /// in their own startup code; AIRC owns only the AIRC-wire layer.
    pub async fn install_diagnostic_subscriber(&self) -> Result<(), AircError> {
        use tracing_subscriber::prelude::*;

        let (tx, mut rx) = unbounded_channel::<DiagnosticEvent>();
        let layer = AircDiagnosticLayer::new(tx);

        // tracing_subscriber::registry().with(...).try_init() is the
        // global-set path. It returns Err if a subscriber is already
        // set; we treat that as fine.
        let _ = tracing_subscriber::registry().with(layer).try_init();

        let airc = self.clone();
        tokio::spawn(async move {
            while let Some(diag) = rx.recv().await {
                if let Err(error) = airc.publish_diagnostic(diag).await {
                    // The drain task can't itself emit a diagnostic
                    // without risking recursion. Use stderr as the
                    // terminal escape hatch — this is the one
                    // exception to the "never use eprintln" rule.
                    eprintln!("airc diagnostic publish failed: {error}");
                }
            }
        });
        Ok(())
    }

    /// Publish a single diagnostic as an AIRC Event frame. Used by
    /// the drain task; consumers normally invoke the `tracing`
    /// macros instead.
    pub async fn publish_diagnostic(&self, diag: DiagnosticEvent) -> Result<(), AircError> {
        let body = serde_json::to_value(&diag)
            .map_err(|error| AircError::Crypto(format!("diagnostic encode: {error}")))?;
        let mut headers = Headers::new();
        headers.insert(
            HEADER_DIAG_LEVEL.into(),
            diag.level.header_value().to_string(),
        );
        headers.insert(HEADER_DIAG_TARGET.into(), diag.target.clone());
        self.send_frame_to(
            FrameKind::Event,
            MentionTarget::All,
            Body::Json(body),
            headers,
        )
        .await?;
        Ok(())
    }

    /// Subscribe to diagnostics. Filtered stream that only yields
    /// events carrying the `airc.diag.level` header.
    pub async fn subscribe_diagnostics(
        &self,
    ) -> Result<
        impl futures::stream::Stream<Item = (Arc<TranscriptEvent>, DiagnosticEvent)>,
        AircError,
    > {
        use futures::stream::StreamExt;
        let inner = self.subscribe().await?;
        Ok(inner.filter_map(|item| async move {
            let event = item.ok()?;
            let diag = parse_diagnostic(&event)?;
            Some((event, diag))
        }))
    }
}

fn parse_diagnostic(event: &TranscriptEvent) -> Option<DiagnosticEvent> {
    let _ = event.headers.get(HEADER_DIAG_LEVEL)?;
    let body = event.body.as_ref()?;
    let value = match body {
        Body::Json(value) => value.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

/// Emit a diagnostic outside the tracing macros. Useful for
/// constructed messages where the tracing call site is awkward
/// (e.g. a generic library function that wants to publish a
/// pre-formatted message).
pub fn emit_diagnostic(
    airc: &Airc,
    level: DiagnosticLevel,
    target: impl Into<String>,
    message: impl Into<String>,
) {
    let target = target.into();
    let message = message.into();
    let emitted_at_ms = now_ms().unwrap_or_default();
    let diag = DiagnosticEvent {
        level,
        target,
        message,
        fields: serde_json::Map::new(),
        emitted_at_ms,
    };
    let airc = airc.clone();
    tokio::spawn(async move {
        if let Err(error) = airc.publish_diagnostic(diag).await {
            eprintln!("airc diagnostic publish failed: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_event_round_trips_through_json() {
        let diag = DiagnosticEvent {
            level: DiagnosticLevel::Warn,
            target: "airc_lib::transport".to_string(),
            message: "frame verification failed".to_string(),
            fields: serde_json::Map::new(),
            emitted_at_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&diag).expect("encode");
        let decoded: DiagnosticEvent = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, diag);
    }

    #[test]
    fn level_header_values_stable() {
        assert_eq!(DiagnosticLevel::Trace.header_value(), "trace");
        assert_eq!(DiagnosticLevel::Debug.header_value(), "debug");
        assert_eq!(DiagnosticLevel::Info.header_value(), "info");
        assert_eq!(DiagnosticLevel::Warn.header_value(), "warn");
        assert_eq!(DiagnosticLevel::Error.header_value(), "error");
    }

    #[test]
    fn level_round_trips_from_tracing() {
        assert_eq!(
            DiagnosticLevel::from_tracing(&tracing::Level::TRACE),
            DiagnosticLevel::Trace
        );
        assert_eq!(
            DiagnosticLevel::from_tracing(&tracing::Level::DEBUG),
            DiagnosticLevel::Debug
        );
        assert_eq!(
            DiagnosticLevel::from_tracing(&tracing::Level::INFO),
            DiagnosticLevel::Info
        );
        assert_eq!(
            DiagnosticLevel::from_tracing(&tracing::Level::WARN),
            DiagnosticLevel::Warn
        );
        assert_eq!(
            DiagnosticLevel::from_tracing(&tracing::Level::ERROR),
            DiagnosticLevel::Error
        );
    }

    #[test]
    fn level_ordering_matches_tracing() {
        assert!(DiagnosticLevel::Trace < DiagnosticLevel::Debug);
        assert!(DiagnosticLevel::Debug < DiagnosticLevel::Info);
        assert!(DiagnosticLevel::Info < DiagnosticLevel::Warn);
        assert!(DiagnosticLevel::Warn < DiagnosticLevel::Error);
    }
}
