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
        self.sync_account_peer_registry().await?;
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
        // Drop the subscribers lock before emitting — the emit
        // path touches the store + broadcast and we don't want
        // to hold the wire subscriber map across an await.
        drop(subs);

        self.emit_wire_established(wire).await?;
        Ok(())
    }

    async fn emit_wire_established(&self, wire: &Path) -> Result<(), AircError> {
        // Resolve channel_name + room_id by matching the wire path
        // against the current subscription set. Best-effort: if no
        // matching subscription is found (e.g. shared-wire test
        // setups, lan subscriber spinning up before any join), emit
        // with the wire path alone and let consumers correlate.
        let (channel_name, room_id) = match self.subscriptions().await {
            Ok(subs) => {
                let canon = wire.canonicalize().ok();
                let matched = subs.into_iter().find(|s| {
                    if let Some(canon) = canon.as_ref() {
                        s.wire.canonicalize().ok().as_ref() == Some(canon)
                    } else {
                        s.wire == wire
                    }
                });
                match matched {
                    Some(sub) => (sub.name.as_str().to_string(), sub.room_id),
                    None => return Ok(()),
                }
            }
            Err(_) => return Ok(()),
        };
        let body = airc_core::Body::Json(
            serde_json::to_value(crate::lifecycle::WireEstablishedBody {
                wire: wire.display().to_string(),
                channel_name,
            })
            .map_err(|e| AircError::Crypto(format!("lifecycle body serialize: {e}")))?,
        );
        self.emit_lifecycle(airc_core::TranscriptKind::WireEstablished, room_id, body)
            .await
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
        let airc = self.clone();
        tokio::spawn(async move {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(frame) => airc.append_received_frame(frame).await,
                    Err(verify_err) => {
                        eprintln!("airc-lib subscriber: frame verification failed: {verify_err}");
                    }
                }
            }
        })
    }

    pub(crate) async fn append_received_frame(&self, frame: Frame) {
        let event = frame.into_transcript_event();
        let event_id = event.event_id;
        // The store dedups persistence by event_id —
        // `DuplicateEventId` just means another writer already
        // persisted this event. Two common cases:
        //
        // 1. SELF: this process sent via `append_sent_frame` (which
        //    already persisted + already broadcast). The wire
        //    subscriber re-reads the same frame ~50ms later. The
        //    `recently_broadcast` ring tells us this event_id was
        //    already fanned out in-process — skip to avoid
        //    double-delivery to local subscribers.
        //
        // 2. CROSS-PROCESS SAME HOME: another scope on the same
        //    `~/.airc/` wrote the frame via its own
        //    `append_sent_frame`. Our store sees DuplicateEventId
        //    because the file is shared (SQLite WAL). The ring is
        //    EMPTY for this event_id in our process — so we DO fan
        //    out. That's how Claude and Codex talking on the same
        //    HOME actually deliver to each other's subscribers.
        match self.inner.store.append(event.clone()).await {
            Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {
                if self.mark_broadcast(event_id) {
                    let _ = self.inner.live_tx.send(event);
                }
            }
            Err(err) => {
                eprintln!("airc-lib subscriber: store append failed: {err}");
            }
        }
    }
}
