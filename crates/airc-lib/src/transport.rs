use std::path::{Path, PathBuf};
use std::sync::Arc;

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_protocol::{Frame, Subscription};
use airc_transport::{SignedTransport, Transport};
use futures::stream::StreamExt;
use tokio::sync::oneshot;

use crate::error::AircError;
use crate::Airc;

pub(crate) struct IngestTask {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl IngestTask {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }
}

impl Drop for IngestTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

pub(crate) struct FrameSubscriber {
    /// Owns the ingest task; dropping aborts it instead of
    /// detaching background work from the SDK handle lifecycle.
    pub(crate) _task: IngestTask,
}

impl Airc {
    pub(crate) async fn ensure_lan_subscriber(&self) -> Result<(), AircError> {
        {
            let subscriber = self.inner.lan_subscriber.lock().await;
            if subscriber.is_some() {
                return Ok(());
            }
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

        let mut subscriber = self.inner.lan_subscriber.lock().await;
        if subscriber.is_some() {
            return Ok(());
        }

        let task = self.spawn_frame_ingest(stream, None, None);
        *subscriber = Some(FrameSubscriber { _task: task });
        Ok(())
    }

    pub(crate) async fn ensure_relay_subscriber(&self) -> Result<(), AircError> {
        {
            let subscriber = self.inner.relay_subscriber.lock().await;
            if subscriber.is_some() {
                return Ok(());
            }
        }

        let adapter = self.relay_adapter().await?;
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

        let mut subscriber = self.inner.relay_subscriber.lock().await;
        if subscriber.is_some() {
            return Ok(());
        }

        let task = self.spawn_frame_ingest(stream, None, None);
        *subscriber = Some(FrameSubscriber { _task: task });
        Ok(())
    }

    pub(crate) async fn ensure_udp_subscriber(&self) -> Result<(), AircError> {
        {
            let subscriber = self.inner.udp_subscriber.lock().await;
            if subscriber.is_some() {
                return Ok(());
            }
        }

        let adapter = self.udp_adapter().await?;
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

        let mut subscriber = self.inner.udp_subscriber.lock().await;
        if subscriber.is_some() {
            return Ok(());
        }

        let task = self.spawn_frame_ingest(stream, None, None);
        *subscriber = Some(FrameSubscriber { _task: task });
        Ok(())
    }

    pub(crate) async fn ensure_webrtc_subscriber(
        &self,
        peer_id: airc_core::PeerId,
    ) -> Result<(), AircError> {
        {
            let subscribers = self.inner.webrtc_subscribers.lock().await;
            if subscribers.contains_key(&peer_id) {
                return Ok(());
            }
        }

        let adapter = self.webrtc_adapter_for(peer_id).await?;
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

        let mut subscribers = self.inner.webrtc_subscribers.lock().await;
        if subscribers.contains_key(&peer_id) {
            return Ok(());
        }

        let task = self.spawn_frame_ingest(stream, None, None);
        subscribers.insert(peer_id, FrameSubscriber { _task: task });
        Ok(())
    }

    /// Spawn the ingest task for a subscription stream.
    ///
    /// - `wire_for_lifecycle = Some(path)` opts the task into
    ///   `WireLost` emission when the stream ends or the shutdown
    ///   signal fires. Pass `None` for streams that don't represent
    ///   a single wire (LAN sweep), since those have no canonical
    ///   wire path to attach the lifecycle event to.
    /// - `shutdown` is consumed; when the corresponding `Sender` is
    ///   dropped or signalled, the task exits with
    ///   `reason="teardown"`. Pass `None` for streams without an
    ///   explicit teardown handle.
    fn spawn_frame_ingest<E>(
        &self,
        mut stream: airc_transport::FrameStream<E>,
        wire_for_lifecycle: Option<PathBuf>,
        shutdown: Option<oneshot::Receiver<()>>,
    ) -> IngestTask
    where
        E: std::fmt::Display + Send + 'static,
    {
        let airc = self.clone();
        IngestTask::new(tokio::spawn(async move {
            let reason: &'static str = match shutdown {
                Some(mut shutdown_rx) => loop {
                    tokio::select! {
                        biased;
                        _ = &mut shutdown_rx => break "teardown",
                        item = stream.next() => match item {
                            Some(Ok(frame)) => airc.append_received_frame(frame).await,
                            Some(Err(verify_err)) => {
                                warn_frame_verify_failed(&verify_err);
                            }
                            None => break "stream_ended",
                        }
                    }
                },
                None => loop {
                    match stream.next().await {
                        Some(Ok(frame)) => airc.append_received_frame(frame).await,
                        Some(Err(verify_err)) => {
                            warn_frame_verify_failed(&verify_err);
                        }
                        None => break "stream_ended",
                    }
                },
            };
            if let Some(wire) = wire_for_lifecycle {
                if let Err(err) = airc.emit_wire_lost(&wire, reason).await {
                    StderrJsonDiagnosticSink.emit(
                        DiagnosticEvent::warn(
                            DiagnosticComponent::Subscriber,
                            DiagnosticCode::WireLostEmitFailed,
                            "wire_lost lifecycle emit failed",
                        )
                        .with_field("wire", wire.display())
                        .with_field("reason", reason)
                        .with_field("error", err),
                    );
                }
            }
        }))
    }

    async fn emit_wire_lost(&self, wire: &Path, reason: &str) -> Result<(), AircError> {
        // Resolve channel/room_id the same way `emit_wire_established`
        // does — by matching the wire against the current
        // subscription set. If the subscription row has already
        // been removed (e.g. by a future `part` flow that races
        // teardown), the emit silently no-ops; consumers see only
        // `WireEstablished` without a matching `WireLost`, which is
        // correct because no room exists for the event to belong to.
        let subs = self.subscriptions().await?;
        let canon = wire.canonicalize().ok();
        let matched = subs.into_iter().find(|s| {
            if let Some(canon) = canon.as_ref() {
                s.wire.canonicalize().ok().as_ref() == Some(canon)
            } else {
                s.wire == wire
            }
        });
        let Some(sub) = matched else {
            return Ok(());
        };
        let room_id = sub.room_id;
        let body = airc_core::Body::Json(
            serde_json::to_value(crate::lifecycle::WireLostBody {
                wire: wire.display().to_string(),
                reason: reason.to_string(),
            })
            .map_err(|e| AircError::Crypto(format!("lifecycle body serialize: {e}")))?,
        );
        self.emit_lifecycle(airc_core::TranscriptKind::WireLost, room_id, body)
            .await
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
        //
        // Card 127816bd Phase 1.C: ring-check first so case 1 (self
        // echo) skips the redundant `store.append`. Saves one full
        // SQLite append + fsync per locally-sent message OFF the
        // wire-tail task — not on the `.say()` critical path (the
        // wire tail is async) but still wasted work that contends
        // for the DB connection with subsequent sends.
        if !self.mark_broadcast(event_id) {
            return;
        }
        match self.inner.store.append(event.clone()).await {
            Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {
                let _ = self.inner.live_tx.send(Arc::new(event));
            }
            Err(err) => {
                StderrJsonDiagnosticSink.emit(
                    DiagnosticEvent::error(
                        DiagnosticComponent::Subscriber,
                        DiagnosticCode::StoreAppendFailed,
                        "subscriber store append failed",
                    )
                    .with_field("event_id", event_id)
                    .with_field("error", err),
                );
            }
        }
    }
}

fn warn_frame_verify_failed(error: &impl std::fmt::Display) {
    if std::env::var_os("AIRC_REPLAY_WARN").is_some() {
        StderrJsonDiagnosticSink.emit(
            DiagnosticEvent::warn(
                DiagnosticComponent::Subscriber,
                DiagnosticCode::FrameVerificationFailed,
                "subscriber frame verification failed",
            )
            .with_field("error", error),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::IngestTask;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn ingest_task_aborts_underlying_task_on_drop() {
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_by_task = Arc::clone(&dropped);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = IngestTask::new(tokio::spawn(async move {
            let _guard = DropFlag(dropped_by_task);
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        }));
        started_rx
            .await
            .expect("spawned ingest task should start before drop assertion");

        drop(task);

        for _ in 0..20 {
            if dropped.load(Ordering::SeqCst) {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            dropped.load(Ordering::SeqCst),
            "dropping IngestTask must abort and drop the spawned future"
        );
    }
}
