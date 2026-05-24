//! Dispatch table — one match arm per `Request` variant.
//!
//! This is the only file that imports both `Request` and `Response`
//! types; every other module either produces or consumes one side.
//! Adding a new op = add Request variant + add arm here. The
//! compiler enforces exhaustiveness.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, RoomId, TranscriptCursor,
};
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_ipc::request::{
    AddPeerRequest, InboxRequest, RemovePeerRequest, Request, SendRequest, SubscribeRequest,
};
use airc_ipc::response::{InboxResponse, PeerEntry, PeersResponse, Response, StatusResponse};
use airc_protocol::{Envelope, Frame, FrameKind, Signature, Subscription};
use airc_transport::Transport;
use futures::stream::StreamExt;

use crate::state::DaemonState;

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
        Request::Attach(_) => Response::Error {
            message: "attach is a streaming request handled by the server".to_string(),
        },
        Request::AddPeer(add) => handle_add_peer(state, add).await,
        Request::RemovePeer(remove) => handle_remove_peer(state, remove).await,
        Request::ListPeers => handle_list_peers(state).await,
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
    let trust_root = match trust_root_for_wire(&sub.wire) {
        Some(root) => root,
        None => {
            return Response::Error {
                message: format!("subscribe: invalid wire path {}", sub.wire.display()),
            };
        }
    };
    match state.refresh_trust_root(&trust_root).await {
        Ok(_) => {}
        Err(error) => {
            return Response::Error {
                message: format!("subscribe: trust refresh: {error}"),
            };
        }
    }
    if state.register_trust_root(&trust_root).await {
        state.spawn_trust_refresher(trust_root);
    }

    // Idempotent: if a subscriber task is already running for this
    // wire, return Ok without spawning a duplicate.
    if !state.register_subscriber(&sub.wire).await {
        return Response::Ok;
    }

    let transport = state.local_fs_for(&sub.wire).await;
    let subscription = Subscription {
        // Replay from the start of the wire — late `Inbox` calls
        // still see pre-existing frames via the store.
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

    let store = state.event_store.clone();
    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            match item {
                Ok(frame) => {
                    let event = frame.into_transcript_event();
                    let event_id = event.event_id;
                    match store.append(event.clone()).await {
                        Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {
                            let _ = state.live_tx.send(event);
                        }
                        Err(err) => {
                            StderrJsonDiagnosticSink.emit(
                                DiagnosticEvent::error(
                                    DiagnosticComponent::Daemon,
                                    DiagnosticCode::StoreAppendFailed,
                                    "daemon subscriber store append failed",
                                )
                                .with_field("event_id", event_id)
                                .with_field("error", err),
                            );
                        }
                    }
                }
                Err(verify_error) => {
                    StderrJsonDiagnosticSink.emit(
                        DiagnosticEvent::warn(
                            DiagnosticComponent::Daemon,
                            DiagnosticCode::FrameVerificationFailed,
                            "daemon subscriber frame verification failed",
                        )
                        .with_field("error", verify_error),
                    );
                }
            }
        }
    });

    Response::Ok
}

fn trust_root_for_wire(wire: &Path) -> Option<PathBuf> {
    match wire.parent() {
        Some(parent) if parent.file_name().is_some_and(|name| name == "wires") => {
            parent.parent().map(Path::to_path_buf)
        }
        _ => Some(wire.to_path_buf()),
    }
}

async fn handle_inbox(state: Arc<DaemonState>, request: InboxRequest) -> Response {
    let limit = request.limit.unwrap_or(INBOX_DEFAULT_LIMIT);
    let events = match request.since.as_ref() {
        Some(cursor) => {
            state
                .event_store
                .resume_from(cursor, request.channel, limit)
                .await
        }
        None => state.event_store.page_recent(request.channel, limit).await,
    };
    let events = match events {
        Ok(events) => events,
        Err(err) => {
            return Response::Error {
                message: format!("inbox: {err}"),
            };
        }
    };
    // Newest cursor in the response is the cursor of the last event
    // returned, so the client can hand it back as `since` next time.
    // Empty page: return None so the caller knows to keep its existing
    // cursor rather than reset to 0.
    let newest = events.last().map(|e| e.cursor());
    Response::Inbox(InboxResponse { events, newest })
}

async fn handle_send(state: Arc<DaemonState>, send: SendRequest) -> Response {
    let transport = state.local_fs_for(&send.wire).await;
    let frame = match build_message_frame(&state, send.channel, &send.text, send.headers) {
        Ok(frame) => frame,
        Err(error) => {
            return Response::Error {
                message: format!("send: clock before UNIX_EPOCH: {error}"),
            };
        }
    };
    match transport.send(frame).await {
        Ok(()) => Response::Ok,
        Err(error) => Response::Error {
            message: format!("send: {error}"),
        },
    }
}

async fn handle_add_peer(state: Arc<DaemonState>, add: AddPeerRequest) -> Response {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let bytes = match URL_SAFE_NO_PAD.decode(&add.pubkey_b64) {
        Ok(b) => b,
        Err(error) => {
            return Response::Error {
                message: format!("add_peer: base64 decode: {error}"),
            };
        }
    };
    if bytes.len() != 32 {
        return Response::Error {
            message: format!("add_peer: pubkey is {} bytes, expected 32", bytes.len()),
        };
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&bytes);
    if let Err(error) = state.registry.enrol(add.peer_id, 0, pubkey) {
        return Response::Error {
            message: format!("add_peer: enrol: {error}"),
        };
    }
    Response::Ok
}

async fn handle_remove_peer(state: Arc<DaemonState>, remove: RemovePeerRequest) -> Response {
    state.registry.remove_peer(remove.peer_id);
    Response::Ok
}

async fn handle_list_peers(state: Arc<DaemonState>) -> Response {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    // We don't currently expose registry iteration on
    // PeerKeyRegistry (only by-peer lookup + find_peer). Read the
    // persisted peer trust store instead — the source of truth that
    // both the daemon and CLI write to.
    let peers = match airc_trust::load(&state.home).await {
        Ok(peers) => peers,
        Err(error) => {
            return Response::Error {
                message: format!("list_peers: {error}"),
            };
        }
    };
    let entries = peers
        .into_iter()
        .map(|p| PeerEntry {
            peer_id: p.peer_id,
            pubkey_b64: p.pubkey_b64,
        })
        .collect();
    // URL_SAFE_NO_PAD pulled in to keep imports stable across future
    // additions (e.g. signed list responses).
    let _ = URL_SAFE_NO_PAD.encode([0u8; 0]);
    Response::Peers(PeersResponse { peers: entries })
}

fn build_message_frame(
    state: &DaemonState,
    channel: uuid::Uuid,
    text: &str,
    headers: Headers,
) -> Result<Frame, std::time::SystemTimeError> {
    let lamport = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    Ok(Frame {
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
            headers,
            body: Some(Body::text(text)),
            media: Vec::new(),
            // Unsigned at this layer — SignedTransport replaces it
            // with Ed25519 on the way out.
            signature: Signature::Unsigned,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::PeerId;
    use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
    use airc_transport::{LocalFsAdapter, SignedTransport};
    use std::path::PathBuf;
    use std::time::Duration;
    use uuid::Uuid;

    fn test_state() -> Arc<DaemonState> {
        let peer_id = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let registry = PeerKeyRegistry::new();
        registry.enrol(peer_id, 0, keypair.public_bytes()).unwrap();
        let registry = Arc::new(registry);
        // Test home is fresh per-call — empty peer trust store is
        // fine for dispatcher tests; no preexisting state required.
        let home = tempfile::TempDir::new().unwrap();
        let home_path = home.path().to_path_buf();
        // Keep the TempDir alive for the test's lifetime by leaking
        // it. (This is a unit-test pattern: cheap, predictable.)
        std::mem::forget(home);
        let store: Arc<dyn airc_store::EventStore> =
            Arc::new(airc_store::InMemoryEventStore::new());
        Arc::new(DaemonState::new(
            peer_id,
            keypair,
            registry,
            VerificationPolicy::Strict,
            home_path,
            store,
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
                headers: Headers::new(),
            }),
        )
        .await;
        assert_eq!(response, Response::Ok);
        // Frame file should exist with one line.
        let frames = dir.path().join("frames.jsonl");
        let contents = std::fs::read_to_string(frames).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    #[test]
    fn trust_root_prefers_airc_wire_root_but_accepts_raw_wire_dirs() {
        let airc_wire = PathBuf::from("/tmp/home/.airc/wires/general");
        assert_eq!(
            trust_root_for_wire(&airc_wire),
            Some(PathBuf::from("/tmp/home/.airc"))
        );

        let raw_wire = PathBuf::from("/tmp/raw-wire-dir");
        assert_eq!(trust_root_for_wire(&raw_wire), Some(raw_wire));
    }

    #[tokio::test]
    async fn subscribed_daemon_learns_wire_root_trust_before_ingest() {
        let root = tempfile::TempDir::new().unwrap();
        let wire_root = root.path().join(".airc");
        let wire = wire_root.join("wires").join("general");

        let daemon = test_state();
        assert_eq!(
            dispatch(
                daemon.clone(),
                Request::Subscribe(SubscribeRequest { wire: wire.clone() }),
            )
            .await,
            Response::Ok
        );

        let sibling_peer = PeerId::from_u128(0xc1a0de);
        let sibling_keypair = PeerKeypair::generate();
        airc_trust::add(&wire_root, sibling_peer, sibling_keypair.public_bytes())
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if daemon.registry.lookup(sibling_peer, 0).is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("daemon verifier must learn sibling trust from the wire root");

        let sender_registry = Arc::new(PeerKeyRegistry::new());
        sender_registry
            .enrol(sibling_peer, 0, sibling_keypair.public_bytes())
            .unwrap();
        let sender_state = DaemonState::new(
            sibling_peer,
            sibling_keypair.clone(),
            sender_registry.clone(),
            VerificationPolicy::Strict,
            wire_root.clone(),
            Arc::new(airc_store::InMemoryEventStore::new()),
        );
        let sender = SignedTransport::new(
            LocalFsAdapter::new(&wire),
            sibling_keypair,
            sibling_peer,
            sender_registry,
            VerificationPolicy::Strict,
        );
        let mut live = daemon.live_tx.subscribe();

        sender
            .send(
                build_message_frame(
                    &sender_state,
                    Uuid::nil(),
                    "sibling scope frame after trust refresh",
                    Headers::new(),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), live.recv())
            .await
            .expect("daemon live stream must receive refreshed-trust frame")
            .expect("live sender must remain open");
        assert_eq!(event.peer_id, sibling_peer);
    }

    #[tokio::test]
    async fn remove_peer_updates_in_memory_registry() {
        let state = test_state();
        let peer_id = PeerId::from_u128(0xb0b);
        let keypair = PeerKeypair::generate();
        let pubkey_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            keypair.public_bytes(),
        );
        assert_eq!(
            dispatch(
                state.clone(),
                Request::AddPeer(AddPeerRequest {
                    peer_id,
                    pubkey_b64,
                }),
            )
            .await,
            Response::Ok
        );
        assert!(state.registry.lookup(peer_id, 0).is_some());

        assert_eq!(
            dispatch(
                state.clone(),
                Request::RemovePeer(RemovePeerRequest { peer_id }),
            )
            .await,
            Response::Ok
        );
        assert!(state.registry.lookup(peer_id, 0).is_none());
    }

    // Pull PathBuf into scope so the import isn't unused when only
    // one test references it (silences clippy without #[allow]).
    #[allow(dead_code)]
    const _UNUSED_TYPE_KEEPALIVE: Option<PathBuf> = None;
}
