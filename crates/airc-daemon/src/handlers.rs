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
use airc_core::scoped_state::{ScopeRef, PEER_IDENTITY_STATE_KEY};
use airc_core::{Body, ClientId, PeerId, RoomId};
use airc_ipc::request::{
    AddPeerRequest, InboxRequest, IpcCursor, IpcTarget, PeerIdentityCardRequest, PresenceRequest,
    PublishRequest, RemovePeerRequest, Request, RoomTipRequest, SendRequest,
};
use airc_ipc::response::{
    InboxResponse, IpcIdentityCard, IpcRoomInfo, PeerEntry, PeerIdentityCardResponse,
    PeersResponse, PresenceResponse, PublishResponse, Response, RoomTipResponse, RoomsResponse,
    RouteEndpointsResponse, StatusResponse,
};
use bytes::Bytes;

use crate::state::DaemonState;

/// Default `Inbox.limit` when the client doesn't pass one. Caps the
/// payload size so a slow client doesn't accidentally pull MB.
const INBOX_DEFAULT_LIMIT: usize = 32;
/// Half the codec's frame cap: the page's envelopes plus the response's own CBOR
/// framing and the cursor must fit under `airc_ipc::codec::MAX_FRAME_BYTES`.
const INBOX_PAGE_BYTE_BUDGET: usize = (airc_ipc::codec::MAX_FRAME_BYTES / 2) as usize;

/// Dispatch one request against the daemon's state. Always returns a
/// Response — Err paths become `Response::Error { message }` so the
/// wire protocol stays uniform.
pub async fn dispatch(state: Arc<DaemonState>, request: Request) -> Response {
    match request {
        Request::Addressed(request) => {
            crate::addressed_connection::route_request(&state, request).await
        }
        Request::AttachAddressed(_) | Request::ProveAddressed(_) => Response::Error {
            message: "addressed proof requires its original challenged connection".into(),
        },
        Request::Ping => Response::Pong,
        Request::Status => {
            let retained = state.router.retention_snapshot();
            Response::Status(StatusResponse {
                peer_id: state.peer_id.to_string(),
                uptime_seconds: state.uptime_seconds(),
                ipc_protocol_version: state.runtime.ipc_protocol_version,
                build_commit: state.runtime.build_commit.clone(),
                build_branch: state.runtime.build_branch.clone(),
                executable: state.runtime.executable.clone(),
                connected_lan_peers: state
                    .connected_lan_peers
                    .load(std::sync::atomic::Ordering::Relaxed),
                connections: Some(state.connections.load(std::sync::atomic::Ordering::Relaxed)),
                channel_count: Some(retained.channel_count),
                ring_entries_total: Some(retained.ring_entries_total),
                ring_bytes_total: Some(retained.ring_bytes_total),
                ring_pinned_total: Some(retained.ring_pinned_total),
                write_behind_queued: Some(retained.write_behind_queued),
                subscriber_queue_depth_total: Some(retained.subscriber_queue_depth_total),
                subscriber_queue_depth_max: Some(retained.subscriber_queue_depth_max),
                // Served by `stream_attach`: one stream fans out a channel set.
                attach_channel_sets: true,
            })
        }
        Request::Send(send) => handle_send(state, send).await,
        Request::Publish(publish) => handle_publish(state, publish).await,
        Request::Inbox(inbox) => handle_inbox(state, inbox).await,
        Request::ScanInbox(scan) => handle_scan_inbox(state, scan).await,
        Request::CountInbox(count) => handle_count_inbox(state, count).await,
        Request::RoomTip(tip) => handle_room_tip(state, tip).await,
        Request::Presence(request) => handle_presence(&state, request),
        Request::PeerIdentityCard(req) => handle_peer_identity_card(state, req).await,
        Request::Attach(_) => Response::Error {
            message: "attach is a streaming request handled by the server".to_string(),
        },
        Request::AddPeer(add) => handle_add_peer(state, add).await,
        Request::RemovePeer(remove) => handle_remove_peer(state, remove).await,
        Request::ListPeers => handle_list_peers(state).await,
        Request::ListRooms => handle_list_rooms(state).await,
        // #1306 slice 2: serve the host-written delivery-ledger snapshot
        // — the delivery-truth read behind doctor's "last confirmed
        // delivery to X: N ago".
        Request::DeliveryStats => {
            Response::DeliveryStats(state.delivery_stats.read().await.clone())
        }
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
    class: airc_core::EventClass,
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
        class,
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
        send.class,
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
    let kind = match crate::kind_map::bus_kind(publish.kind) {
        Ok(kind) => kind,
        Err(refusal) => {
            return Response::Error {
                message: format!("publish: {refusal}"),
            }
        }
    };
    publish_envelope(
        &state,
        publish.channel,
        (
            PeerId::from_uuid(publish.from_peer),
            ClientId::from_uuid(publish.from_client),
        ),
        kind,
        publish.class,
        crate::kind_map::bus_delivery(publish.delivery),
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
    // Continuum #297: the kinds filter is applied BEFORE `limit`, so
    // the page is "the newest N events OF THESE KINDS" — never the
    // survivors of a raw newest-N (three personas streaming ~4
    // StreamChunk frames/sec evict every Message from a 50-event page,
    // deafening working personas to direction; post-filtering
    // client-side yields crumbs). `None` AND the empty vec both mean
    // unfiltered: filtering to nothing is never what a caller means,
    // so an empty list is treated as absent rather than answered with
    // a vacuously empty page.
    let kinds: Option<Vec<Kind>> = match request
        .kinds
        .as_ref()
        .filter(|kinds| !kinds.is_empty())
        .map(|kinds| crate::kind_map::bus_kinds(kinds.iter().copied()))
        .transpose()
    {
        Ok(kinds) => kinds,
        Err(refusal) => {
            return Response::Error {
                message: format!("inbox: {refusal}"),
            }
        }
    };
    if request.since.is_some() && request.before.is_some() {
        return Response::Error {
            message: "inbox: `since` pages forward and `before` pages back; pass one".to_string(),
        };
    }
    if let Some(b) = request.before {
        // History, backward: the newest `limit` durable events strictly
        // before the cursor, from the store, on demand. The read path behind
        // an attach's one streamed page.
        let before = Some(Cursor::new(Seq::new(b.epoch, b.counter), b.event_id));
        let page = match &kinds {
            None => {
                state
                    .router
                    .durable_tail_before(channel, before, limit)
                    .await
            }
            Some(kinds) => {
                state
                    .router
                    .durable_tail_before_of_kinds(channel, before, kinds, limit)
                    .await
            }
        };
        return match page {
            Ok(events) => inbox_page_response(&events, limit, true),
            Err(error) => Response::Error {
                message: format!("inbox: {error}"),
            },
        };
    }
    let events = match request.since {
        // "Most recent N" when no cursor (card 8428ae8c): reverse-paged
        // at the store layer — work bounded by N (ring tail + at most
        // one indexed `page_tail`), NEVER a full-room replay truncated
        // in memory. `durable_tail` is durable-only by contract; the
        // kinds-filtered leg rides the same merge with the kind
        // predicate pushed into both the ring scan and the sink query.
        None => {
            let tail = match &kinds {
                None => state.router.durable_tail(channel, limit).await,
                Some(kinds) => {
                    state
                        .router
                        .durable_tail_of_kinds(channel, kinds, limit)
                        .await
                }
            };
            match tail {
                Ok(events) => events,
                Err(error) => {
                    return Response::Error {
                        message: format!("inbox: {error}"),
                    }
                }
            }
        }
        // "First N after the cursor" when resuming. Each router page is
        // bounded by `limit` (one indexed query), and the durable + kinds
        // predicates are applied per page; a page they thin below `limit`
        // pulls the next page from the last raw cursor, so a short answer
        // still means "the channel is exhausted" to the client's paging loop.
        Some(c) => {
            let mut from = Some(Cursor::new(Seq::new(c.epoch, c.counter), c.event_id));
            let mut events: Vec<Arc<Envelope>> = Vec::new();
            loop {
                let batch = match state.router.resume_from_cursor(channel, from, limit).await {
                    Ok(events) => events,
                    Err(error) => {
                        return Response::Error {
                            message: format!("inbox: {error}"),
                        }
                    }
                };
                let exhausted = batch.len() < limit;
                let last_raw = batch.last().map(|e| e.cursor());
                // `inbox` is the DURABLE transcript. `resume_from_cursor`
                // merges the hot ring (which transiently holds every class,
                // incl. StreamChunk / EphemeralLatest for live attach-replay)
                // with the sink, so filter to durable here — non-durable
                // classes never belong in a replay (§3.4). The kinds filter
                // composes the same way.
                events.extend(batch.into_iter().filter(|env| {
                    env.delivery.is_durable()
                        && kinds.as_ref().is_none_or(|kinds| kinds.contains(&env.kind))
                }));
                if events.len() >= limit || exhausted {
                    break;
                }
                match last_raw {
                    Some(cursor) => from = Some(cursor),
                    None => break,
                }
            }
            events.truncate(limit);
            events
        }
    };
    inbox_page_response(&events, limit, request.since.is_some())
}

/// One inbox page, encoded within the byte budget. `paging` = the caller is
/// walking pages (a `since` or `before` cursor), so a full page means there
/// may be more in that direction.
fn inbox_page_response(events: &[Arc<Envelope>], limit: usize, paging: bool) -> Response {
    let page_full = events.len() >= limit;
    let (envelopes, kept, cut) = encode_page_within_budget(events, INBOX_PAGE_BYTE_BUDGET);
    let newest = kept.last().map(|e| {
        let cursor = e.cursor();
        IpcCursor {
            epoch: cursor.seq.epoch,
            counter: cursor.seq.counter,
            event_id: cursor.event_id,
        }
    });
    Response::Inbox(InboxResponse {
        envelopes,
        newest,
        has_more: cut || (page_full && paging),
    })
}

/// A page is bounded by BYTES as well as by count: the IPC codec refuses a frame
/// over `MAX_FRAME_BYTES` and drops the connection, so a page that would exceed it
/// is not "large", it is undeliverable — and a client that re-asks the same page
/// re-fails forever (the M5, 2026-09-16: 1024 × ~23 KB remote-inference prompts in
/// one room = a 23.9 MB page, refused every 6 s for six hours). Keep the oldest
/// envelopes that fit under `budget` (the cursor order the caller resumes from),
/// always at least one so a single oversize event still moves the cursor; report
/// whether the page was cut.
fn encode_page_within_budget(
    events: &[Arc<Envelope>],
    budget: usize,
) -> (Vec<Vec<u8>>, Vec<Arc<Envelope>>, bool) {
    let mut out = Vec::with_capacity(events.len());
    let mut kept = Vec::with_capacity(events.len());
    let mut bytes = 0usize;
    for e in events {
        let enc = airc_wire::encode(e).to_vec();
        if !out.is_empty() && bytes + enc.len() > budget {
            return (out, kept, true);
        }
        bytes += enc.len();
        out.push(enc);
        kept.push(Arc::clone(e));
    }
    (out, kept, false)
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

/// airc#1341: the channel's live presence, straight from the router's
/// ephemeral cache — one entry per `coalesce_key`, TTL-expired entries already
/// gone. Presence is state: no durable read, no replay, nothing decoded here.
fn handle_presence(state: &DaemonState, request: PresenceRequest) -> Response {
    Response::Presence(PresenceResponse {
        envelopes: state
            .router
            .ephemeral_snapshot(request.channel)
            .iter()
            .map(|env| airc_wire::encode(env).to_vec())
            .collect(),
    })
}

/// Resolve one peer's durable identity card from the daemon's owner-core
/// identity index (`scoped_state`, `user:<peer>` / `identity.card`). The
/// identity analog of [`handle_room_tip`]: an attached client's local
/// store never observes foreign peers' cards (they are indexed here, off
/// the broadcast `IdentityPublished` event), so name resolution
/// (`peer_alias` / `peer_identity_card` / `room_roster`) reads the
/// daemon's index over IPC instead of replaying the room. A store error
/// is surfaced loudly; an absent row is an honest `card: None`, not an
/// error.
async fn handle_peer_identity_card(
    state: Arc<DaemonState>,
    request: PeerIdentityCardRequest,
) -> Response {
    let scope_key = ScopeRef::User(request.peer_id).scope_key();
    match state
        .coordinator_store
        .get_scoped_state(&scope_key, PEER_IDENTITY_STATE_KEY)
        .await
    {
        Ok(stored) => Response::PeerIdentityCard(PeerIdentityCardResponse {
            card: stored.map(|row| IpcIdentityCard {
                value_json: row.value_json,
                version: row.version,
            }),
        }),
        Err(error) => Response::Error {
            message: format!("peer_identity_card: {error}"),
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
    if let Err(error) =
        crate::trust_refresh::refresh_peer(&state.registry, &state.home, add.peer_id, Some(pubkey))
            .await
    {
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

/// #270/#241: serve the durable subscribed-room registry from the
/// coordinator store. Parted rooms are excluded — the store keeps them
/// only so auto-restore doesn't resurrect an explicit leave. This is
/// THE membership read: nav/room-list clients seed from it instead of
/// inferring rooms from observed traffic (the seam that left a rebooted
/// interface showing one room until each of the others happened to
/// speak).
async fn handle_list_rooms(state: Arc<DaemonState>) -> Response {
    match state.coordinator_store.load_subscriptions().await {
        Ok(rows) => Response::Rooms(RoomsResponse {
            rooms: rows
                .into_iter()
                .filter(|row| !row.parted)
                .map(|row| IpcRoomInfo {
                    room_id: row.room_id,
                    name: row.channel_name,
                    joined_at_ms: row.joined_at_ms,
                    is_default: row.is_default,
                })
                .collect(),
        }),
        Err(error) => Response::Error {
            message: format!("list_rooms: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload_len: usize) -> Arc<Envelope> {
        Arc::new(Envelope::new(
            RoomId::from_u128(7),
            (PeerId::from_u128(1), ClientId::from_u128(2)),
            Kind::Message,
            airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            DeliveryClass::Durable,
            Bytes::from(vec![b'x'; payload_len]),
        ))
    }

    // what this catches (M5, 2026-09-16): a 1024-event page of ~23 KB prompts is a
    // 23.9 MB frame the codec refuses — the daemon answered it every 6 s for six hours,
    // dropping the client each time. A page is cut at the byte budget, keeps the OLDEST
    // events (the resume order), reports `has_more`, and a single oversize event still
    // moves the cursor instead of wedging the client on it forever.
    #[test]
    fn a_page_is_cut_at_the_byte_budget_and_says_so() {
        let events: Vec<Arc<Envelope>> = (0..10).map(|_| envelope(1000)).collect();
        let (out, kept, cut) = encode_page_within_budget(&events, 3_500);
        assert!(cut, "ten 1 KB events do not fit a 3.5 KB budget");
        assert_eq!(out.len(), kept.len());
        assert!(
            out.len() >= 2 && out.len() <= 3,
            "kept the oldest that fit: {}",
            out.len()
        );
        assert!(
            Arc::ptr_eq(&kept[0], &events[0]),
            "resume order: the oldest first"
        );
        let (all, _, cut) = encode_page_within_budget(&events, 1 << 20);
        assert!(!cut && all.len() == 10, "a page under budget is whole");
        let huge = vec![envelope(64 * 1024)];
        let (one, _, cut) = encode_page_within_budget(&huge, 100);
        assert_eq!(
            (one.len(), cut),
            (1, false),
            "one oversize event is still delivered so the cursor moves"
        );
    }
}

async fn handle_scan_inbox(
    state: Arc<DaemonState>,
    request: airc_ipc::request::ScanInboxRequest,
) -> Response {
    let cursor = |c: IpcCursor| Cursor::new(Seq::new(c.epoch, c.counter), c.event_id);
    let after = cursor(request.after);
    let through = cursor(request.through);
    if request.limit == 0 || request.limit > 4096 || after.is_after(&through) {
        return Response::Error {
            message: "scan inbox: invalid bounds or limit (1..4096)".into(),
        };
    }
    let events = match state
        .router
        .matching_history(
            request.channel,
            after,
            through,
            airc_core::HistoryMatch {
                selection: request.selection.clone(),
                kinds: &request.kinds,
                self_echo: request.self_echo,
                headers: &request.headers,
                addressed_to: request.addressed_to,
            },
            request.limit + 1,
            request.tail,
        )
        .await
    {
        Ok(events) => events,
        Err(error) => {
            return Response::Error {
                message: format!("scan inbox: {error}"),
            }
        }
    };
    scan_page_response(events, request, airc_ipc::codec::MAX_FRAME_BYTES as usize)
}

async fn handle_count_inbox(
    state: Arc<DaemonState>,
    request: airc_ipc::request::CountInboxRequest,
) -> Response {
    let cursor = |c: IpcCursor| Cursor::new(Seq::new(c.epoch, c.counter), c.event_id);
    let after = cursor(request.after);
    let through = cursor(request.through);
    if after.is_after(&through) {
        return Response::Error {
            message: "count inbox: `after` is past `through`".into(),
        };
    }
    match state
        .router
        .count_matching(
            request.channel,
            after,
            through,
            airc_core::HistoryMatch {
                selection: request.selection.clone(),
                kinds: &request.kinds,
                self_echo: request.self_echo,
                headers: &request.headers,
                addressed_to: request.addressed_to,
            },
            request.exclude_self.as_ref(),
        )
        .await
    {
        Ok(count) => Response::CountInbox(count),
        Err(error) => Response::Error {
            message: format!("count inbox: {error}"),
        },
    }
}

fn scan_page_response(
    mut events: Vec<Arc<Envelope>>,
    request: airc_ipc::request::ScanInboxRequest,
    budget: usize,
) -> Response {
    let mut more = events.len() > request.limit;
    if request.tail && more {
        events.remove(0);
    } else {
        events.truncate(request.limit);
    }
    if request.tail {
        more = false;
    }
    let mut envelopes = Vec::new();
    let mut bytes = 0;
    let mut last = request.after;
    for event in events {
        let encoded = airc_wire::encode(&event);
        let next = IpcCursor {
            epoch: event.seq.epoch,
            counter: event.seq.counter,
            event_id: event.event_id,
        };
        let array_bytes = airc_ipc::codec::byte_array_payload_len(&encoded);
        // Reserve the larger of complete and truncated cursor metadata: either
        // outcome must fit without reserializing all earlier envelope bodies.
        let overhead = [next, request.through]
            .into_iter()
            .map(|cursor| {
                airc_ipc::codec::scan_inbox_payload_overhead(cursor, true, envelopes.len() + 1)
            })
            .collect::<std::io::Result<Vec<_>>>();
        let overhead = match overhead {
            Ok(sizes) => sizes.into_iter().max().unwrap_or(0),
            Err(error) => {
                return Response::Error {
                    message: format!("scan inbox framing: {error}"),
                }
            }
        };
        if bytes + array_bytes + overhead > budget.min(airc_ipc::codec::MAX_FRAME_BYTES as usize) {
            if envelopes.is_empty() || request.tail {
                return Response::Error {
                    message: "scan inbox: event exceeds response budget".into(),
                };
            }
            more = true;
            break;
        }
        bytes += array_bytes;
        envelopes.push(encoded.to_vec());
        last = IpcCursor {
            epoch: event.seq.epoch,
            counter: event.seq.counter,
            event_id: event.event_id,
        };
    }
    // Tail intentionally omits earlier history, but must never acknowledge a byte-cut later match.
    let scanned_through = if more { last } else { request.through };
    Response::ScanInbox(airc_ipc::response::ScanInboxResponse {
        envelopes,
        scanned_through,
        has_more: more,
    })
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    #[test]
    fn maximum_cursor_and_high_bytes_fit_real_codec_or_fail_without_progress() {
        let room = RoomId::from_u128(1);
        let make = |counter, size| {
            let mut event = Envelope::new(
                room,
                (PeerId::from_u128(2), ClientId::from_u128(3)),
                Kind::Message,
                airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
                DeliveryClass::Durable,
                Bytes::from(vec![255; size]),
            );
            event.seq = Seq::new(u64::MAX, counter);
            Arc::new(event)
        };
        let cap = airc_ipc::codec::MAX_FRAME_BYTES as usize;
        let request = airc_ipc::request::ScanInboxRequest {
            selection: airc_core::EventSelection::All,
            kinds: std::collections::BTreeSet::new(),
            self_echo: None,
            channel: room,
            after: IpcCursor {
                epoch: u64::MAX,
                counter: 0,
                event_id: airc_core::EventId::from_u128(0),
            },
            through: IpcCursor {
                epoch: u64::MAX,
                counter: u64::MAX,
                event_id: airc_core::EventId::from_u128(u128::MAX),
            },
            headers: airc_core::HeaderFilter::Any,
            addressed_to: None,
            limit: 2,
            tail: false,
        };
        let response = scan_page_response(
            vec![make(1, cap / 4), make(2, cap / 4)],
            request.clone(),
            cap,
        );
        let Response::ScanInbox(page) = &response else {
            panic!("bounded page expected")
        };
        assert_eq!(page.envelopes.len(), 1);
        assert!(page.has_more);
        assert_eq!(page.scanned_through.counter, 1);
        assert!(airc_ipc::codec::encode_frame_payload(&response).is_ok());
        let oversized = scan_page_response(vec![make(1, cap / 2)], request.clone(), cap);
        assert!(matches!(oversized, Response::Error { .. }));
        let mut tail = request;
        tail.tail = true;
        assert!(matches!(
            scan_page_response(vec![make(1, cap / 4), make(2, cap / 4)], tail, cap),
            Response::Error { .. }
        ));
    }

    #[tokio::test]
    async fn byte_cut_acknowledges_only_the_delivered_prefix_and_empty_scan_reaches_bound() {
        let room = RoomId::from_u128(1);
        let events: Vec<_> = (1..=3)
            .map(|n| {
                let mut e = Envelope::new(
                    room,
                    (PeerId::from_u128(2), ClientId::from_u128(3)),
                    Kind::Message,
                    airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
                    DeliveryClass::Durable,
                    Bytes::from(vec![0; 1024]),
                );
                e.seq = Seq::new(1, n);
                Arc::new(e)
            })
            .collect();
        let request = airc_ipc::request::ScanInboxRequest {
            selection: airc_core::EventSelection::All,
            kinds: std::collections::BTreeSet::new(),
            self_echo: None,
            channel: room,
            after: IpcCursor {
                epoch: 1,
                counter: 0,
                event_id: airc_core::EventId::from_u128(0),
            },
            through: IpcCursor {
                epoch: 1,
                counter: 9,
                event_id: airc_core::EventId::from_u128(9),
            },
            headers: airc_core::HeaderFilter::Any,
            addressed_to: None,
            limit: 3,
            tail: false,
        };
        let budget = airc_ipc::codec::byte_array_payload_len(&airc_wire::encode(&events[0]))
            + airc_ipc::codec::scan_inbox_payload_overhead(request.through, true, 1).unwrap();
        let Response::ScanInbox(page) = scan_page_response(events, request.clone(), budget) else {
            panic!("scan response expected")
        };
        assert_eq!(page.envelopes.len(), 1);
        assert!(page.has_more);
        assert_eq!(page.scanned_through.counter, 1);
        let Response::ScanInbox(empty) = scan_page_response(vec![], request.clone(), budget) else {
            panic!("scan response expected")
        };
        assert_eq!(empty.scanned_through, request.through);
        assert!(!empty.has_more);
        // An ordinary Inbox reply cannot be mistaken for the positive ScanInbox contract.
        let mut encoded = Vec::new();
        airc_ipc::codec::write_frame(
            &mut encoded,
            &Response::Inbox(InboxResponse {
                envelopes: vec![],
                newest: None,
                has_more: false,
            }),
        )
        .await
        .unwrap();
        assert!(!matches!(
            airc_ipc::codec::read_frame::<_, Response>(&mut encoded.as_slice())
                .await
                .unwrap()
                .unwrap(),
            Response::ScanInbox(_)
        ));
    }
}
