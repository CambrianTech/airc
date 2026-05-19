//! Dispatch table — one match arm per `Request` variant.
//!
//! This is the only file that imports both `Request` and `Response`
//! types; every other module either produces or consumes one side.
//! Adding a new op = add Request variant + add arm here. The
//! compiler enforces exhaustiveness.

use std::sync::Arc;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, RoomId, TranscriptCursor,
};
use airc_protocol::{Envelope, Frame, FrameKind, Signature, Subscription};
use airc_transport::Transport;
use futures::stream::StreamExt;

use crate::daemon::state::DaemonState;
use crate::ipc::request::{InboxRequest, Request, SendRequest, SubscribeRequest};
use crate::ipc::response::{InboxResponse, Response, StatusResponse};

/// Default `Inbox.limit` when the client doesn't pass one. Caps the
/// payload size so a slow client doesn't accidentally pull MB.
const INBOX_DEFAULT_LIMIT: usize = 32;

/// Dispatch one request against the daemon's state. Always returns a
/// Response — Err paths become `Response::Error { message }` so the
/// wire protocol stays uniform.
pub async fn dispatch(state: Arc<DaemonState>, request: Request) -> Response {
    match request {
        Request::Ping => Response::Pong,
        Request::Status => Response::Status(StatusResponse {
            peer_id: state.peer_id.to_string(),
            uptime_seconds: state.uptime_seconds(),
        }),
        Request::Send(send) => handle_send(state, send).await,
        Request::Subscribe(sub) => handle_subscribe(state, sub).await,
        Request::Inbox(inbox) => handle_inbox(state, inbox).await,
        Request::Stop => {
            // Don't actually stop here; just signal. The server's
            // accept loop watches the same notifier and exits after
            // sending this response.
            state.shutdown.notify_waiters();
            Response::Ok
        }
    }
}

async fn handle_subscribe(state: Arc<DaemonState>, sub: SubscribeRequest) -> Response {
    // Idempotent: if the inbox already exists, the subscriber task is
    // already draining frames into it. Return Ok without spawning a
    // duplicate.
    if state.has_inbox(&sub.wire).await {
        return Response::Ok;
    }

    // Create the inbox buffer + spawn the drain task.
    let buffer = state.inbox_for(&sub.wire).await;
    let transport = state.local_fs_for(&sub.wire).await;
    let subscription = Subscription {
        // Replay from the start of the wire — buffer captures
        // pre-existing frames so an inbox call seconds later still
        // sees them.
        from_cursor: Some(TranscriptCursor {
            lamport: 0,
            event_id: EventId::from_u128(0),
        }),
        ..Default::default()
    };

    let mut stream = match transport.subscribe(subscription).await {
        Ok(stream) => stream,
        Err(error) => {
            return Response::Error {
                message: format!("subscribe: {error}"),
            };
        }
    };

    let buffer_for_task = buffer.clone();
    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            match item {
                Ok(frame) => {
                    buffer_for_task.lock().await.push(frame);
                }
                Err(_verify_error) => {
                    // Verification failure — don't buffer.
                    // Future: surface a counter in Status response.
                }
            }
        }
    });

    Response::Ok
}

async fn handle_inbox(state: Arc<DaemonState>, request: InboxRequest) -> Response {
    let Some(buffer) = state.inboxes.lock().await.get(&request.wire).cloned() else {
        // No subscription on this wire — return empty inbox rather
        // than an error so clients can call Inbox before Subscribe
        // without an awkward warmup race.
        return Response::Inbox(InboxResponse {
            frames: Vec::new(),
            newest_lamport: request.since_lamport.unwrap_or(0),
        });
    };
    let buffer = buffer.lock().await;
    let limit = request.limit.unwrap_or(INBOX_DEFAULT_LIMIT);
    let frames = buffer.since(request.since_lamport, limit);
    // newest_lamport in the response is the max we return so the
    // client can hand it back as since_lamport next time. If we
    // returned no frames, echo the input cursor so the client doesn't
    // restart from 0.
    let newest_lamport = frames
        .iter()
        .map(|f| f.envelope.lamport)
        .max()
        .or(request.since_lamport)
        .unwrap_or_else(|| buffer.newest_lamport());
    Response::Inbox(InboxResponse {
        frames,
        newest_lamport,
    })
}

async fn handle_send(state: Arc<DaemonState>, send: SendRequest) -> Response {
    let transport = state.local_fs_for(&send.wire).await;
    let frame = build_message_frame(&state, send.channel, &send.text);
    match transport.send(frame).await {
        Ok(()) => Response::Ok,
        Err(error) => Response::Error {
            message: format!("send: {error}"),
        },
    }
}

fn build_message_frame(state: &DaemonState, channel: uuid::Uuid, text: &str) -> Frame {
    let lamport = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Frame {
        kind: FrameKind::Message,
        envelope: Envelope {
            event_id: EventId::new(),
            sender: state.peer_id,
            sender_client: ClientId::new(),
            channel: RoomId::from_uuid(channel),
            target: MentionTarget::All,
            lamport,
            occurred_at_ms: lamport,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text(text)),
            media: Vec::new(),
            // Unsigned at this layer — SignedTransport replaces it
            // with Ed25519 on the way out.
            signature: Signature::Unsigned,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::PeerId;
    use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
    use std::path::PathBuf;
    use std::sync::RwLock;
    use uuid::Uuid;

    fn test_state() -> Arc<DaemonState> {
        let peer_id = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer_id, 0, keypair.public_bytes()).unwrap();
        let registry = Arc::new(RwLock::new(registry));
        Arc::new(DaemonState::new(
            peer_id,
            keypair,
            registry,
            VerificationPolicy::Strict,
        ))
    }

    #[tokio::test]
    async fn ping_returns_pong() {
        let state = test_state();
        let response = dispatch(state, Request::Ping).await;
        assert_eq!(response, Response::Pong);
    }

    #[tokio::test]
    async fn status_carries_peer_id_and_uptime() {
        let state = test_state();
        // Sleep so uptime > 0 — pins that the field is wired.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let response = dispatch(state.clone(), Request::Status).await;
        match response {
            Response::Status(status) => {
                assert_eq!(status.peer_id, state.peer_id.to_string());
                assert!(status.uptime_seconds >= 1, "uptime must accumulate");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stop_signals_shutdown_and_returns_ok() {
        let state = test_state();
        // Clone the Arc so we can hand one to dispatch (consumed) and
        // keep one to subscribe on the shutdown notifier (borrowed).
        let dispatch_state = state.clone();
        // Subscribe to the shutdown notifier before dispatching so
        // notify_waiters wakes us. (Notify drops notifications sent
        // before any waiter exists; the test pin is that AFTER a
        // listener is set up, dispatch(Stop) wakes it.)
        let listener = state.shutdown.notified();
        tokio::pin!(listener);

        let response = dispatch(dispatch_state, Request::Stop).await;
        assert_eq!(response, Response::Ok);

        // The notifier should fire promptly — yield once to let the
        // signal propagate.
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut listener)
            .await
            .expect("shutdown signal must fire after Stop");
    }

    #[tokio::test]
    async fn send_dispatches_against_local_fs_transport() {
        // End-to-end through dispatch: a Send request hits the
        // (lazily-created) local-fs adapter and Ok comes back.
        let dir = tempfile::TempDir::new().unwrap();
        let state = test_state();
        let response = dispatch(
            state,
            Request::Send(SendRequest {
                wire: dir.path().to_path_buf(),
                channel: Uuid::nil(),
                text: "hello".to_string(),
            }),
        )
        .await;
        assert_eq!(response, Response::Ok);
        // Frame file should exist with one line.
        let frames = dir.path().join("frames.jsonl");
        let contents = std::fs::read_to_string(frames).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    // Pull PathBuf into scope so the import isn't unused when only
    // one test references it (silences clippy without #[allow]).
    #[allow(dead_code)]
    const _UNUSED_TYPE_KEEPALIVE: Option<PathBuf> = None;
}
