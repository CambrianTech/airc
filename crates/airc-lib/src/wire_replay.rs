use std::path::Path;

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_protocol::{verify, Frame};

use crate::error::AircError;
use crate::Airc;

const FRAMES_FILENAME: &str = "frames.jsonl";

impl Airc {
    pub(crate) async fn replay_wire_once(&self, wire: &Path) -> Result<(), AircError> {
        self.sync_account_peer_registry().await?;
        let path = wire.join(FRAMES_FILENAME);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(AircError::Transport(error.to_string())),
        };

        let mut malformed_count = 0usize;
        let mut unverifiable_count = 0usize;

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let frame: Frame = match serde_json::from_str(trimmed) {
                Ok(frame) => frame,
                Err(error) => {
                    // A single malformed line shouldn't abort the
                    // whole replay — surface it and continue, same
                    // policy as the live frame-ingest task. Wire
                    // files can carry frames from older builds or
                    // ad-hoc test fixtures; refusing to read past
                    // one means losing every later real event too.
                    malformed_count += 1;
                    if replay_warnings_enabled() {
                        StderrJsonDiagnosticSink.emit(
                            DiagnosticEvent::warn(
                                DiagnosticComponent::Replay,
                                DiagnosticCode::MalformedReplayFrameSkipped,
                                "skipping malformed replay frame",
                            )
                            .with_field("wire_file", path.display())
                            .with_field("error", error),
                        );
                    }
                    continue;
                }
            };
            let verify_result = verify(&frame, self.inner.policy, self.inner.registry.as_ref());
            if let Err(error) = verify_result {
                // Same fail-open policy as `spawn_frame_ingest` in
                // transport.rs:91. The most common case is a frame
                // signed by a peer who's no longer enrolled (e.g.
                // an orphan identity from a previous install on the
                // same wire). Failing closed here aborts every
                // subsequent legitimate frame in the same file
                // and breaks `airc inbox` / `airc join` for any
                // scope that touched the wire pre-identity-reset.
                unverifiable_count += 1;
                if replay_warnings_enabled() {
                    StderrJsonDiagnosticSink.emit(
                        DiagnosticEvent::warn(
                            DiagnosticComponent::Replay,
                            DiagnosticCode::UnverifiableReplayFrameSkipped,
                            "skipping unverifiable replay frame",
                        )
                        .with_field("wire_file", path.display())
                        .with_field("error", error),
                    );
                }
                continue;
            }
            let event = frame.into_transcript_event();
            match self.inner.store.append(event).await {
                Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        if replay_warnings_enabled() && (malformed_count > 0 || unverifiable_count > 0) {
            StderrJsonDiagnosticSink.emit(
                DiagnosticEvent::warn(
                    DiagnosticComponent::Replay,
                    DiagnosticCode::ReplayFramesSkipped,
                    "replay skipped invalid frame(s)",
                )
                .with_field("wire_file", path.display())
                .with_field("malformed_count", malformed_count)
                .with_field("unverifiable_count", unverifiable_count),
            );
        }
        Ok(())
    }

    pub(crate) async fn replay_subscribed_wires_once(&self) -> Result<(), AircError> {
        for wire in self.subscribed_wires().await? {
            self.replay_wire_once(&wire).await?;
        }
        Ok(())
    }
}

fn replay_warnings_enabled() -> bool {
    replay_warnings_enabled_from(std::env::var_os("AIRC_REPLAY_WARN"))
}

fn replay_warnings_enabled_from(value: Option<std::ffi::OsString>) -> bool {
    value.is_some()
}

#[cfg(test)]
mod tests {
    use super::replay_warnings_enabled_from;

    #[test]
    fn replay_warnings_default_to_quiet() {
        assert!(!replay_warnings_enabled_from(None));
    }

    #[test]
    fn replay_warnings_can_be_enabled_for_diagnostics() {
        assert!(replay_warnings_enabled_from(Some("1".into())));
    }
}
