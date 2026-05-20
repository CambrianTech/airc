use airc_core::{Body, EventId, Headers, MentionTarget, TranscriptCursor, TranscriptEvent};
use airc_protocol::{Envelope, Frame, FrameKind, Signature};
use tokio_stream::wrappers::BroadcastStream;

use crate::error::AircError;
use crate::route::{RouteClass, RouteDecision, TransportResolver, TransportRoute};
use crate::stream::{EventFilter, EventStream, FilteredEventStream};
use crate::time::now_ms;
use crate::Airc;

impl Airc {
    /// Send a plain-text message to the current room.
    pub async fn say(&self, text: &str) -> Result<EventId, AircError> {
        if self.is_daemon_attached() {
            let room = self.current_room().await?;
            return self.daemon_send_text(&room, text).await;
        }
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
        let route = self.resolve_send_route(kind)?;
        let event_id = EventId::new();
        let occurred_at_ms = now_ms()?;
        let mut frame = Frame {
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
        frame.envelope.signature = self
            .inner
            .identity
            .keypair
            .sign_envelope(&frame.envelope, self.inner.identity.peer_id, 0)
            .map_err(|error| AircError::Crypto(error.to_string()))?;
        self.execute_send_route(route.kind, &room, frame.clone())
            .await?;
        self.append_sent_frame(frame).await?;
        Ok(event_id)
    }

    fn resolve_send_route(&self, kind: FrameKind) -> Result<TransportRoute, AircError> {
        let class = route_class_for_frame(kind);
        let samples = self
            .inner
            .route_health
            .read()
            .map_err(|_| AircError::Route("route health lock poisoned".to_string()))?
            .samples();
        TransportResolver::from_health(samples)
            .resolve(class)
            .map_err(format_route_refusal)
    }

    async fn append_sent_frame(&self, frame: Frame) -> Result<(), AircError> {
        let event = frame.into_transcript_event();
        match self.inner.store.append(event.clone()).await {
            Ok(()) => {
                let _ = self.inner.live_tx.send(event);
                Ok(())
            }
            Err(airc_store::StoreError::DuplicateEventId(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
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

    /// Subscribe to live events matching `filter`. If the filter does
    /// not specify a channel, it is scoped to the current room.
    pub async fn subscribe_filtered(
        &self,
        filter: EventFilter,
    ) -> Result<FilteredEventStream, AircError> {
        let filter = self.scope_filter_to_current_room(filter).await?;
        Ok(FilteredEventStream {
            inner: self.subscribe().await?,
            filter,
        })
    }

    /// Fetch the most recent `limit` events from the current room.
    pub async fn page_recent(&self, limit: usize) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        if self.is_daemon_attached() {
            return self.daemon_page_recent(&room, limit).await;
        }
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(self
            .inner
            .store
            .page_recent(Some(room.channel), limit)
            .await?)
    }

    /// Fetch recent events matching `filter`. If the filter does not
    /// specify a channel, it is scoped to the current room.
    pub async fn page_recent_filtered(
        &self,
        filter: EventFilter,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let filter = self.scope_filter_to_current_room(filter).await?;
        self.ensure_current_room_subscriber().await?;
        Ok(self
            .inner
            .store
            .page_recent(filter.channel, limit)
            .await?
            .into_iter()
            .filter(|event| filter.matches(event))
            .collect())
    }

    /// Fetch up to `limit` events strictly after `cursor`.
    pub async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        if self.is_daemon_attached() {
            return self.daemon_resume_from(&room, cursor, limit).await;
        }
        Ok(self
            .inner
            .store
            .resume_from(cursor, Some(room.channel), limit)
            .await?)
    }

    /// Fetch events strictly after `cursor` that match `filter`. If
    /// the filter does not specify a channel, it is scoped to the
    /// current room.
    pub async fn resume_from_filtered(
        &self,
        cursor: &TranscriptCursor,
        filter: EventFilter,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let filter = self.scope_filter_to_current_room(filter).await?;
        Ok(self
            .inner
            .store
            .resume_from(cursor, filter.channel, limit)
            .await?
            .into_iter()
            .filter(|event| filter.matches(event))
            .collect())
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

    async fn scope_filter_to_current_room(
        &self,
        mut filter: EventFilter,
    ) -> Result<EventFilter, AircError> {
        if filter.channel.is_none() {
            filter.channel = Some(self.current_room().await?.channel);
        }
        Ok(filter)
    }

    async fn ensure_current_room_subscriber(&self) -> Result<(), AircError> {
        let room = self.current_room().await?;
        self.ensure_wire_subscriber(&room.wire).await
    }
}

fn route_class_for_frame(kind: FrameKind) -> RouteClass {
    match kind {
        FrameKind::Message => RouteClass::DataInteractive,
        FrameKind::Event | FrameKind::Control => RouteClass::ControlInteractive,
    }
}

fn format_route_refusal(decision: RouteDecision) -> AircError {
    match decision {
        RouteDecision::NoRoute { class } => {
            AircError::Route(format!("{class:?} has no admissible live route"))
        }
        RouteDecision::Selected(kind) => AircError::Route(format!(
            "unexpected selected route returned as refusal: {kind:?}"
        )),
    }
}
