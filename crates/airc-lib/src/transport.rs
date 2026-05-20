use std::path::Path;

use airc_core::{EventId, TranscriptCursor};
use airc_protocol::{Frame, Subscription};
use airc_transport::{LocalFsAdapter, SignedTransport, Transport};
use futures::stream::StreamExt;

use crate::error::AircError;
use crate::room::Room;
use crate::Airc;

pub(crate) struct WireSubscriber {
    /// Kept alive by ownership of its `JoinHandle`.
    pub(crate) _task: tokio::task::JoinHandle<()>,
}

pub(crate) struct FrameSubscriber {
    /// Kept alive by ownership of its `JoinHandle`.
    pub(crate) _task: tokio::task::JoinHandle<()>,
}

impl Airc {
    pub(crate) async fn ensure_room_subscriber(&self, room: &Room) -> Result<(), AircError> {
        self.ensure_wire_subscriber(&room.wire).await
    }

    pub(crate) async fn ensure_wire_subscriber(&self, wire: &Path) -> Result<(), AircError> {
        let mut subs = self.inner.subscribers.lock().await;
        if subs.contains_key(wire) {
            return Ok(());
        }
        let transport = SignedTransport::new(
            LocalFsAdapter::new(wire),
            self.inner.identity.keypair.clone(),
            self.inner.identity.peer_id,
            self.inner.registry.clone(),
            self.inner.policy,
        );
        let subscription = Subscription {
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let stream = transport
            .subscribe(subscription)
            .await
            .map_err(|e| AircError::Transport(e.to_string()))?;

        let task = self.spawn_frame_ingest(stream);
        subs.insert(wire.to_path_buf(), WireSubscriber { _task: task });
        Ok(())
    }

    pub(crate) async fn ensure_lan_subscriber(&self) -> Result<(), AircError> {
        let mut subscriber = self.inner.lan_subscriber.lock().await;
        if subscriber.is_some() {
            return Ok(());
        }
        let adapter = self.lan_adapter().await?;
        let transport = SignedTransport::new(
            adapter,
            self.inner.identity.keypair.clone(),
            self.inner.identity.peer_id,
            self.inner.registry.clone(),
            self.inner.policy,
        );
        let subscription = Subscription {
            from_cursor: None,
            ..Default::default()
        };
        let stream = transport
            .subscribe(subscription)
            .await
            .map_err(|e| AircError::Transport(e.to_string()))?;
        let task = self.spawn_frame_ingest(stream);
        *subscriber = Some(FrameSubscriber { _task: task });
        Ok(())
    }

    fn spawn_frame_ingest<E>(
        &self,
        mut stream: airc_transport::FrameStream<E>,
    ) -> tokio::task::JoinHandle<()>
    where
        E: std::fmt::Display + Send + 'static,
    {
        let store = self.inner.store.clone();
        let live_tx = self.inner.live_tx.clone();
        tokio::spawn(async move {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(frame) => append_received_frame(frame, store.as_ref(), &live_tx).await,
                    Err(verify_err) => {
                        eprintln!("airc-lib subscriber: frame verification failed: {verify_err}");
                    }
                }
            }
        })
    }
}

async fn append_received_frame(
    frame: Frame,
    store: &dyn airc_store::EventStore,
    live_tx: &tokio::sync::broadcast::Sender<airc_core::TranscriptEvent>,
) {
    let event = frame.into_transcript_event();
    match store.append(event.clone()).await {
        Ok(()) => {
            let _ = live_tx.send(event);
        }
        Err(airc_store::StoreError::DuplicateEventId(_)) => {}
        Err(err) => {
            eprintln!("airc-lib subscriber: store append failed: {err}");
        }
    }
}
