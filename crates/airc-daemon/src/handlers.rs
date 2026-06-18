//! Dispatch table — one match arm per `Request` variant.
//!
//! This is the only file that imports both `Request` and `Response`
//! types; every other module either produces or consumes one side.
//! Adding a new op = add Request variant + add arm here. The compiler
//! enforces exhaustiveness.
//!
//! Owner-core model: `Send`/`Publish` author an `airc_bus::Envelope` and
//! hand it to the daemon's `EventRouter`; `Inbox` resumes from the
//! router (hot ring + SQLite durable tier, no gap). There is no wire,
//! no per-subscriber verify, no file poll. Live events leave via the
//! `Attach` stream in `server.rs`.

use std::sync::Arc;

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind, Target};
use airc_bus::{Clock, Seq, SystemClock};
use airc_core::{Body, ClientId, PeerId, RoomId};
use airc_ipc::request::{
    AddPeerRequest, InboxRequest, IpcCursor, IpcDelivery, IpcKind, IpcTarget, PublishRequest,
    RemovePeerRequest, Request, RoomTipRequest, SendRequest,
};
use airc_ipc::response::{
    InboxResponse, PeerEntry, PeersResponse, PublishResponse, Response, RoomTipResponse,
    RouteEndpointsResponse, StatusResponse,
};
use bytes::Bytes;

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
            ipc_protocol_version: state.runtime.ipc_protocol_version,
            build_commit: state.runtime.build_commit.clone(),
            build_branch: state.runtime.build_branch.clone(),
            executable: state.runtime.executable.clone(),
            connected_lan_peers: state
                .connected_lan_peers
                .load(std::sync::atomic::Ordering::Relaxed),
        }),
        Request::Send(send) => handle_send(state, send).await,
        Request::Publish(publish) => handle_publish(state, publish).await,
        Request::Inbox(inbox) => handle_inbox(state, inbox).await,
        Request::RoomTip(tip) => handle_room_tip(state, tip).await,
        Request::Attach(_) => Response::Error {
            message: "attach is a streaming request handled by the server".to_string(),
        },
        Request::AddPeer(add) => handle_add_peer(state, add).await,
        Request::RemovePeer(remove) => handle_remove_peer(state, remove).await,
        Request::ListPeers => handle_list_peers(state).await,
        // Card 4b6a0ffa (#33): serve the endpoints the registry glue
        // recorded after binding its listener. Empty means "up but not
        // dialable" — the client decides what that implies.
        Request::RouteEndpoints => Response::RouteEndpoints(RouteEndpointsResponse {
            endpoints: state.route_endpoints.read().await.clone(),
        }),
        Request::Stop => {
            // Don't actually stop here; just signal. The server's accept
            // loop watches the same notifier and exits after sending
            // this response.
            state.shutdown.notify_waiters();
            Response::Ok
        }
    }
}

fn map_kind(kind: IpcKind) -> Kind {
    match kind {
        IpcKind::Message => Kind::Message,
        IpcKind::Event => Kind::Event,
        IpcKind::Command => Kind::Command,
        IpcKind::CommandResult => Kind::CommandResult,
        IpcKind::Signal => Kind::Signal,
        IpcKind::StreamChunk => Kind::StreamChunk,
        IpcKind::Control => Kind::Control,
    }
}

fn map_delivery(delivery: IpcDelivery) -> DeliveryClass {
    match delivery {
        IpcDelivery::Durable => DeliveryClass::Durable,
        IpcDelivery::EphemeralLatest => DeliveryClass::EphemeralLatest,
        IpcDelivery::EphemeralWindow => DeliveryClass::EphemeralWindow,
        IpcDelivery::RequestResponse => DeliveryClass::RequestResponse,
        IpcDelivery::StreamChunk => DeliveryClass::StreamChunk,
    }
}

fn map_target(target: IpcTarget) -> Target {
    match target {
        IpcTarget::All => Target::All,
        IpcTarget::Peer(peer) => Target::Peer(peer),
        IpcTarget::Endpoint(name) => Target::Endpoint(name),
        IpcTarget::Reply(id) => Target::Reply(id),
        IpcTarget::Capability(cap) => Target::Capability(cap),
    }
}

/// Author a bus envelope from the IPC fields and publish it through the
/// router. The router stamps `(epoch, counter)` + `occurred_at` and
/// fans out (and, for `Durable`, write-behinds to the ORM).
#[allow(clippy::too_many_arguments)]
async fn publish_envelope(
    state: &DaemonState,
    channel: uuid::Uuid,
    from: (PeerId, ClientId),
    kind: Kind,
    delivery: DeliveryClass,
    target: Target,
    correlation_id: Option<uuid::Uuid>,
    coalesce_key: Option<String>,
    payload: Vec<u8>,
    headers: airc_core::Headers,
) -> Response {
    let channel = RoomId::from_uuid(channel);
    let mut env = Envelope::new(
        channel,
        // Broker, not author: the envelope carries the originating
        // participant's identity (supplied by the attached client), not
        // the daemon's machine peer. That keeps per-agent attribution
        // intact across the one-daemon-per-machine fan-out.
        from,
        kind,
        delivery,
        // Opaque consumer bytes — moved straight into the envelope; the
        // daemon never parses the payload (boundary-encoding discipline).
        Bytes::from(payload),
    );
    env.target = target;
    env.correlation_id = correlation_id;
    env.coalesce_key = coalesce_key;
    env.headers = headers;
    let event_id = env.event_id;
    let occurred_at_ms = SystemClock.now_ms();
    match state.router.publish(env).await {
        Ok(seq) => Response::Publish(PublishResponse {
            event_id,
            epoch: seq.epoch,
            counter: seq.counter,
            occurred_at_ms,
            channel_id: channel,
        }),
        Err(error) => Response::Error {
            message: format!("publish: {error}"),
        },
    }
}

async fn handle_send(state: Arc<DaemonState>, send: SendRequest) -> Response {
    // Chat text is the one place the daemon authors a payload codec:
    // the canonical `{"text":...}` `Body`. Everything structured/raw
    // arrives pre-encoded via `Publish`.
    publish_envelope(
        &state,
        send.channel,
        (
            PeerId::from_uuid(send.from_peer),
            ClientId::from_uuid(send.from_client),
        ),
        Kind::Message,
        DeliveryClass::Durable,
        Target::All,
        None,
        None,
        Body::text(send.text).to_payload(),
        send.headers,
    )
    .await
}

async fn handle_publish(state: Arc<DaemonState>, publish: PublishRequest) -> Response {
    publish_envelope(
        &state,
        publish.channel,
        (
            PeerId::from_uuid(publish.from_peer),
            ClientId::from_uuid(publish.from_client),
        ),
        map_kind(publish.kind),
        map_delivery(publish.delivery),
        map_target(publish.target),
        publish.correlation_id,
        publish.coalesce_key,
        publish.payload,
        publish.headers,
    )
    .await
}

async fn handle_inbox(state: Arc<DaemonState>, request: InboxRequest) -> Response {
    // The owner-core router pages per channel — durable replay is always
    // scoped to a room (no global transcript table to scan).
    let channel = match request.channel {
        Some(channel) => channel,
        None => {
            return Response::Error {
                message: "inbox requires a channel in the owner-core model".to_string(),
            }
        }
    };
    let limit = request.limit.unwrap_or(INBOX_DEFAULT_LIMIT);
    let events = match request.since {
        // "Most recent N" when no cursor (card 8428ae8c): reverse-paged
        // at the store layer — work bounded by N (ring tail + at most
        // one indexed `page_tail`), NEVER a full-room replay truncated
        // in memory. `durable_tail` is durable-only by contract.
        None => match state.router.durable_tail(channel, limit).await {
            Ok(events) => events,
            Err(error) => {
                return Response::Error {
                    message: format!("inbox: {error}"),
                }
            }
        },
        // "First N after the cursor" when resuming.
        Some(c) => {
            let from = Some(Cursor::new(Seq::new(c.epoch, c.counter), c.event_id));
            let mut events = match state.router.resume_from_cursor(channel, from).await {
                Ok(events) => events,
                Err(error) => {
                    return Response::Error {
                        message: format!("inbox: {error}"),
                    }
                }
            };
            // `inbox` is the DURABLE transcript. `resume_from_cursor`
            // merges the hot ring (which transiently holds every class,
            // incl. StreamChunk / EphemeralLatest for live attach-replay)
            // with the sink, so filter to durable here — non-durable
            // classes never belong in a replay (§3.4).
            events.retain(|env| env.delivery.is_durable());
            events.truncate(limit);
            events
        }
    };
    let envelopes: Vec<Vec<u8>> = events
        .iter()
        .map(|e| airc_wire::encode(e).to_vec())
        .collect();
    let newest = events.last().map(|e| {
        let cursor = e.cursor();
        IpcCursor {
            epoch: cursor.seq.epoch,
            counter: cursor.seq.counter,
            event_id: cursor.event_id,
        }
    });
    Response::Inbox(InboxResponse { envelopes, newest })
}

/// Card a1562dbc: the O(1) tip probe. Answered by the router's
/// `durable_tip` — hot-ring newest-durable, else one indexed sink row —
/// NEVER by replaying the room. A store error is surfaced loudly; there
/// is no scan fallback.
async fn handle_room_tip(state: Arc<DaemonState>, request: RoomTipRequest) -> Response {
    match state.router.durable_tip(request.channel).await {
        Ok(tip) => Response::RoomTip(RoomTipResponse {
            tip: tip.map(|cursor| IpcCursor {
                epoch: cursor.seq.epoch,
                counter: cursor.seq.counter,
                event_id: cursor.event_id,
            }),
        }),
        Err(error) => Response::Error {
            message: format!("room_tip: {error}"),
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
    Response::Peers(PeersResponse { peers: entries })
}
