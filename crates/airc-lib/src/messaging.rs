use airc_core::{Body, EventId, Headers, MentionTarget, TranscriptCursor, TranscriptEvent};
use airc_protocol::{Envelope, Frame, FrameKind, Signature};
use airc_transport::{LocalFsAdapter, SignedTransport, Transport};
use tokio_stream::wrappers::BroadcastStream;

use crate::error::AircError;
use crate::stream::EventStream;
use crate::time::now_ms;
use crate::Airc;

impl Airc {
    /// Send a plain-text message to the current room.
    pub async fn say(&self, text: &str) -> Result<EventId, AircError> {
        self.send(Body::text(text), Headers::new()).await
    }

    /// Send a frame with typed body and arbitrary headers.
    pub async fn send(&self, body: Body, headers: Headers) -> Result<EventId, AircError> {
        self.send_frame(FrameKind::Message, body, headers).await
    }

    pub(crate) async fn send_frame(
        &self,
        kind: FrameKind,
        body: Body,
        headers: Headers,
    ) -> Result<EventId, AircError> {
        let room = self.current_room().await?;
        self.ensure_wire_subscriber(&room.wire).await?;
        let event_id = EventId::new();
        let occurred_at_ms = now_ms();
        let frame = Frame {
            kind,
            envelope: Envelope {
                event_id,
                sender: self.inner.identity.peer_id,
                sender_client: self.inner.identity.client_id,
                channel: room.channel,
                target: MentionTarget::All,
                lamport: occurred_at_ms,
                occurred_at_ms,
                reply_to: None,
                headers,
                body: Some(body),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        };
        let transport = SignedTransport::new(
            LocalFsAdapter::new(&room.wire),
            self.inner.identity.keypair.clone(),
            self.inner.identity.peer_id,
            self.inner.registry.clone(),
            self.inner.policy,
        );
        transport
            .send(frame)
            .await
            .map_err(|e| AircError::Transport(e.to_string()))?;
        Ok(event_id)
    }

    /// Subscribe to the live event stream.
    pub async fn subscribe(&self) -> Result<EventStream, AircError> {
        let room = self.current_room().await?;
        let rx = self.inner.live_tx.subscribe();
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(EventStream {
            inner: BroadcastStream::new(rx),
        })
    }

    /// Fetch the most recent `limit` events from the current room.
    pub async fn page_recent(&self, limit: usize) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(self
            .inner
            .store
            .page_recent(Some(room.channel), limit)
            .await?)
    }

    /// Fetch up to `limit` events strictly after `cursor`.
    pub async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        Ok(self
            .inner
            .store
            .resume_from(cursor, Some(room.channel), limit)
            .await?)
    }

    /// Cursor of the newest event in the current room.
    pub async fn latest_cursor(&self) -> Result<Option<TranscriptCursor>, AircError> {
        let room = self.current_room().await?;
        Ok(self.inner.store.latest_cursor(Some(room.channel)).await?)
    }

    /// Append a `TranscriptEvent` to the durable store directly.
    pub async fn append_event(&self, event: TranscriptEvent) -> Result<(), AircError> {
        Ok(self.inner.store.append(event).await?)
    }
}
