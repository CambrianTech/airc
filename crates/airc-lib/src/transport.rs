use std::path::{Path, PathBuf};
use std::sync::Arc;

use airc_core::{EventId, TranscriptCursor};
use airc_protocol::{Frame, Subscription};
use airc_transport::{LocalFsAdapter, SignedTransport, Transport};
use futures::stream::StreamExt;
use tokio::sync::oneshot;

use crate::error::AircError;
use crate::room::Room;
use crate::Airc;

pub(crate) struct WireSubscriber {
    /// Kept alive by ownership of its `JoinHandle`.
    pub(crate) _task: tokio::task::JoinHandle<()>,
    /// Drop or `take().send(())` to stop the task and trigger the
    /// `WireLost` emit path with `reason="teardown"`. `None` after
    /// the sender has been consumed by a teardown call.
    pub(crate) shutdown: Option<oneshot::Sender<()>>,
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

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task = self.spawn_frame_ingest(stream, Some(wire.to_path_buf()), Some(shutdown_rx));
        subs.insert(
            wire.to_path_buf(),
            WireSubscriber {
                _task: task,
                shutdown: Some(shutdown_tx),
            },
        );
        // Drop the subscribers lock before emitting — the emit
        // path touches the store + broadcast and we don't want
        // to hold the wire subscriber map across an await.
        drop(subs);

        self.emit_wire_established(wire).await?;
        Ok(())
    }

    /// Test-only public alias for [`Airc::teardown_wire`]. Kept
    /// behind `#[doc(hidden)]` so it isn't part of the documented
    /// SDK surface — the eventual public verb is owned by the
    /// `airc part` / `airc teardown` CLI lanes.
    #[doc(hidden)]
    pub async fn teardown_wire_for_test(&self, wire: &Path) -> Result<(), AircError> {
        self.teardown_wire(wire).await
    }

    /// Stop tailing `wire`. Drops the local subscriber and emits a
    /// `WireLost` lifecycle event with `reason="teardown"`. No-op if
    /// the wire is not currently subscribed.
    pub(crate) async fn teardown_wire(&self, wire: &Path) -> Result<(), AircError> {
        let mut subs = self.inner.subscribers.lock().await;
        let Some(mut subscriber) = subs.remove(wire) else {
            return Ok(());
        };
        let shutdown = subscriber.shutdown.take();
        // Drop the map lock before awaiting the task — the task may
        // be mid-emit and needs the broadcast channel.
        drop(subs);
        if let Some(tx) = shutdown {
            // `Err` only fires if the receiver is already gone (task
            // exited on its own). In that path the task already
            // emitted WireLost with `reason="stream_ended"`; nothing
            // more to do here.
            let _ = tx.send(());
        }
        // Wait briefly for the task to finalize its emit. Bounded so
        // a wedged subscriber can't hang teardown forever.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), subscriber._task).await;
        Ok(())
    }

    async fn emit_wire_established(&self, wire: &Path) -> Result<(), AircError> {
        // Resolve channel_name + room_id by matching the wire path
        // against the current subscription set. Missing match is a
        // legitimate no-op for shared-wire test setups; failure to
        // read the subscription set propagates.
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
        let (channel_name, room_id) = (sub.name.as_str().to_string(), sub.room_id);
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
        let task = self.spawn_frame_ingest(stream, None, None);
        *subscriber = Some(FrameSubscriber { _task: task });
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
    ) -> tokio::task::JoinHandle<()>
    where
        E: std::fmt::Display + Send + 'static,
    {
        let airc = self.clone();
        tokio::spawn(async move {
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
                    eprintln!(
                        "airc-lib subscriber: wire_lost emit failed for {} ({reason}): {err}",
                        wire.display()
                    );
                }
            }
        })
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
        match self.inner.store.append(event.clone()).await {
            Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {
                if self.mark_broadcast(event_id) {
                    let _ = self.inner.live_tx.send(Arc::new(event));
                }
            }
            Err(err) => {
                eprintln!("airc-lib subscriber: store append failed: {err}");
            }
        }
    }
}

fn warn_frame_verify_failed(error: &impl std::fmt::Display) {
    if std::env::var_os("AIRC_REPLAY_WARN").is_some() {
        eprintln!("airc-lib subscriber: frame verification failed: {error}");
    }
}
