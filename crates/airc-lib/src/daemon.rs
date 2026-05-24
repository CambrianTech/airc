//! Daemon-attached SDK mode.
//!
//! Consumers still use `Airc`; attach mode routes operations through
//! the daemon's typed IPC client instead of making apps construct
//! daemon requests directly.

use airc_core::{EventId, Headers, TranscriptCursor, TranscriptEvent};
use airc_ipc::{InboxRequest, SendRequest, SubscribeRequest};

use crate::error::AircError;
use crate::room::Room;
use crate::Airc;

impl Airc {
    pub(crate) fn daemon_client(&self) -> Option<&airc_ipc::DaemonClient> {
        self.inner.daemon_client.as_deref()
    }

    pub(crate) async fn daemon_send_text(
        &self,
        room: &Room,
        text: &str,
        headers: Headers,
    ) -> Result<EventId, AircError> {
        self.daemon_client()
            .ok_or_else(|| AircError::Route("daemon client is not attached".to_string()))?
            .send(SendRequest {
                wire: room.wire.clone(),
                channel: room.channel.as_uuid(),
                text: text.to_string(),
                headers,
            })
            .await?;

        // Current daemon Send response is an ack-only protocol. Until
        // the daemon returns the event id, expose a local id so the
        // API remains fallible/synchronous without inventing a second
        // response path in the CLI.
        Ok(EventId::new())
    }

    pub(crate) async fn daemon_page_recent(
        &self,
        room: &Room,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        self.daemon_subscribe_room(room).await?;
        Ok(self
            .daemon_client()
            .ok_or_else(|| AircError::Route("daemon client is not attached".to_string()))?
            .inbox(InboxRequest {
                since: None,
                channel: Some(room.channel),
                limit: Some(limit),
            })
            .await?
            .events)
    }

    pub(crate) async fn daemon_resume_from(
        &self,
        room: &Room,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        self.daemon_subscribe_room(room).await?;
        Ok(self
            .daemon_client()
            .ok_or_else(|| AircError::Route("daemon client is not attached".to_string()))?
            .inbox(InboxRequest {
                since: Some(cursor.clone()),
                channel: Some(room.channel),
                limit: Some(limit),
            })
            .await?
            .events)
    }

    pub(crate) async fn daemon_subscribe_room(&self, room: &Room) -> Result<(), AircError> {
        self.daemon_client()
            .ok_or_else(|| AircError::Route("daemon client is not attached".to_string()))?
            .subscribe(SubscribeRequest {
                wire: room.wire.clone(),
            })
            .await?;
        Ok(())
    }
}
