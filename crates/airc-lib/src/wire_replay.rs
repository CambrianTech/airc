use std::path::Path;

use airc_protocol::{verify, Frame};

use crate::error::AircError;
use crate::Airc;

const FRAMES_FILENAME: &str = "frames.jsonl";

impl Airc {
    pub(crate) async fn replay_wire_once(&self, wire: &Path) -> Result<(), AircError> {
        self.sync_account_peer_registry()?;
        let path = wire.join(FRAMES_FILENAME);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(AircError::Transport(error.to_string())),
        };

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let frame: Frame = serde_json::from_str(trimmed)
                .map_err(|error| AircError::Transport(error.to_string()))?;
            {
                let registry = self
                    .inner
                    .registry
                    .read()
                    .map_err(|_| AircError::Crypto("registry lock poisoned".to_string()))?;
                verify(&frame, self.inner.policy, &registry)
                    .map_err(|error| AircError::Crypto(error.to_string()))?;
            }
            let event = frame.into_transcript_event();
            match self.inner.store.append(event).await {
                Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {}
                Err(error) => return Err(error.into()),
            }
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
