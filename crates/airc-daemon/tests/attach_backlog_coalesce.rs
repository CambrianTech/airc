//! Attach never replays history down a stream (Joel, 2026-10-06: "it should
//! be impossible"). `Live` is the live edge; every start in the past (a
//! bookmark, or the transcript start, whatever flags it carries) streams ONE
//! page of the newest events and a summary counting the rest, and
//! `Inbox { before }` pages older history backward on demand. (Card 7d5b6a65
//! introduced the summary frame this keeps.)
//!
//! Why this matters (Joel directive 2026-05-29): the agent-Monitor
//! pattern (live attention-routing) breaks when every fresh attach
//! replays days of transcript and fires one notification per
//! historical event. The doctrine for `AttachRequest::from = None`
//! said "starts from the live edge" but the implementation returned
//! the whole ring; this card splits that intent: explicit `from_now`
//! for the live-tail shape, explicit `coalesce_backlog` for the
//! summary-frame catch-up shape.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_core::{Headers, PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::codec::read_frame;
use airc_ipc::{
    AttachRequest, AttachStart, ChannelAttach, DaemonClient, InboxRequest, IpcCursor, IpcDelivery,
    IpcKind, IpcTarget, PublishRequest, Response,
};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use tokio::task::JoinHandle;

struct TestDaemon {
    socket: PathBuf,
    handle: JoinHandle<()>,
    peer_id: PeerId,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-abc-{}-{n}.sock", std::process::id()))
}

async fn start_daemon() -> TestDaemon {
    let home = tempfile::TempDir::new().expect("tempdir");
    let db_path = home.path().join("events.sqlite");
    let peer_id = PeerId::new();
    let keypair = PeerKeypair::generate();
    let registry = PeerKeyRegistry::new();
    registry
        .enrol(peer_id, 0, keypair.public_bytes())
        .expect("enrol self");
    let coordinator: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let state = Arc::new(
        DaemonState::build(
            peer_id,
            keypair,
            Arc::new(registry),
            VerificationPolicy::Strict,
            home.path().to_path_buf(),
            &db_path,
            coordinator,
            DaemonRuntimeInfo::unknown(),
        )
        .await
        .expect("build daemon state"),
    );
    let socket = unique_socket();
    let server_state = state.clone();
    let server_socket = socket.clone();
    let handle = tokio::spawn(async move {
        let _ = run(server_state, server_socket).await;
    });
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    TestDaemon {
        socket,
        handle,
        peer_id,
        _home: home,
    }
}

impl TestDaemon {
    async fn stop(self) {
        let _ = DaemonClient::new(self.socket.clone()).stop().await;
        let _ = tokio::time::timeout(Duration::from_secs(3), self.handle).await;
    }
}

/// Publish `n` payloads through the daemon so they land in the ring +
/// sink; the next attach will see them as backlog.
async fn publish_n(daemon: &TestDaemon, channel: RoomId, n: usize) -> Vec<IpcCursor> {
    let client = DaemonClient::new(daemon.socket.clone());
    let from_client = uuid::Uuid::new_v4();
    let mut cursors = Vec::with_capacity(n);
    for i in 0..n {
        let receipt = client
            .publish(PublishRequest {
                channel: channel.as_uuid(),
                from_peer: daemon.peer_id.as_uuid(),
                from_client,
                target: IpcTarget::All,
                kind: IpcKind::Message,
                delivery: IpcDelivery::Durable,
                correlation_id: None,
                coalesce_key: None,
                payload: format!("backlog event {i}").into_bytes(),
                headers: Headers::new(),
            })
            .await
            .expect("publish");
        cursors.push(IpcCursor {
            epoch: receipt.epoch,
            counter: receipt.counter,
            event_id: receipt.event_id,
        });
    }
    cursors
}

/// Read the next frame off an attach stream with a timeout, panicking
/// with `context` on timeout/eof so failures name the phase they died in.
async fn next_frame(stream: &mut (impl tokio::io::AsyncRead + Unpin), context: &str) -> Response {
    tokio::time::timeout(Duration::from_secs(3), read_frame::<_, Response>(stream))
        .await
        .unwrap_or_else(|_| panic!("timeout waiting for frame: {context}"))
        .expect("frame")
        .unwrap_or_else(|| panic!("stream closed waiting for frame: {context}"))
}

/// Publish one live event so the daemon's catch-up seam flushes.
async fn publish_live(daemon: &TestDaemon, channel: RoomId, payload: &[u8]) {
    DaemonClient::new(daemon.socket.clone())
        .publish(PublishRequest {
            channel: channel.as_uuid(),
            from_peer: daemon.peer_id.as_uuid(),
            from_client: uuid::Uuid::new_v4(),
            target: IpcTarget::All,
            kind: IpcKind::Message,
            delivery: IpcDelivery::Durable,
            correlation_id: None,
            coalesce_key: None,
            payload: payload.to_vec(),
            headers: Headers::new(),
        })
        .await
        .expect("publish live");
}

/// Card 7d5b6a65 acceptance: `from_now: true` sends NO historical
/// envelopes, only events published strictly after the attach call
/// returns.
#[tokio::test]
async fn attach_from_now_skips_full_backlog() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    publish_n(&daemon, channel, 30).await;
    // Small breather so the ring is fully populated before attach.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(AttachRequest::new(channel, AttachStart::Live))
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }

    // No event for a generous window. If the daemon were replaying
    // backlog we'd see 30 Event frames here.
    match tokio::time::timeout(
        Duration::from_millis(300),
        read_frame::<_, Response>(&mut stream),
    )
    .await
    {
        Err(_) => { /* timeout = no backlog delivered, expected */ }
        Ok(Ok(Some(Response::Event { .. }))) => {
            panic!("attach from_now=true must not deliver backlog events")
        }
        Ok(Ok(Some(Response::AttachCursorAdvanced { .. }))) => {
            panic!("attach from_now=true must not deliver a catch-up summary either")
        }
        Ok(other) => panic!("unexpected frame on from_now stream: {other:?}"),
    }

    // Now publish a LIVE event; it MUST arrive.
    let live_client = DaemonClient::new(daemon.socket.clone());
    let from_client = uuid::Uuid::new_v4();
    live_client
        .publish(PublishRequest {
            channel: channel.as_uuid(),
            from_peer: daemon.peer_id.as_uuid(),
            from_client,
            target: IpcTarget::All,
            kind: IpcKind::Message,
            delivery: IpcDelivery::Durable,
            correlation_id: None,
            coalesce_key: None,
            payload: b"live event".to_vec(),
            headers: Headers::new(),
        })
        .await
        .expect("publish live");

    let live = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<_, Response>(&mut stream),
    )
    .await
    .expect("live event arrives")
    .expect("frame")
    .expect("Some");
    match live {
        Response::Event { envelope } => {
            let env = airc_wire::decode(envelope.into()).expect("decode");
            assert_eq!(env.payload.to_vec(), b"live event".to_vec());
        }
        other => panic!("expected Event frame for live, got {other:?}"),
    }
    daemon.stop().await;
}

/// `publish_live`, returning the event's cursor.
async fn publish_live_at(daemon: &TestDaemon, channel: RoomId, payload: &[u8]) -> IpcCursor {
    let receipt = DaemonClient::new(daemon.socket.clone())
        .publish(PublishRequest {
            channel: channel.as_uuid(),
            from_peer: daemon.peer_id.as_uuid(),
            from_client: uuid::Uuid::new_v4(),
            target: IpcTarget::All,
            kind: IpcKind::Message,
            delivery: IpcDelivery::Durable,
            correlation_id: None,
            coalesce_key: None,
            payload: payload.to_vec(),
            headers: Headers::new(),
        })
        .await
        .expect("publish live");
    IpcCursor {
        epoch: receipt.epoch,
        counter: receipt.counter,
        event_id: receipt.event_id,
    }
}

/// The newest page the daemon streams on any start in the past.
const PAGE: usize = 10;

/// Read `n` Event frames, returning their payloads in order.
async fn read_events(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    n: usize,
    context: &str,
) -> Vec<String> {
    let mut payloads = Vec::with_capacity(n);
    for i in 0..n {
        match next_frame(stream, &format!("{context}: event {i}")).await {
            Response::Event { envelope } => {
                let env = airc_wire::decode(envelope.into()).expect("decode");
                payloads.push(String::from_utf8(env.payload.to_vec()).expect("utf8"));
            }
            other => panic!("{context}: expected Event {i}, got {other:?}"),
        }
    }
    payloads
}

async fn attach_ok(daemon: &TestDaemon, request: AttachRequest) -> airc_ipc::transport::IpcStream {
    let mut stream = DaemonClient::new(daemon.socket.clone())
        .attach(request)
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => stream,
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }
}

// what this catches (Joel, 2026-10-06: replaying history down a stream
// "should be impossible"; attach "anchors at NOW and pages BACKWARD"): every
// start in the past, whatever flags it carries, streams at most one page of
// the newest events, oldest first, then ONE summary counting what it left
// out, on an IDLE room (no live event needed to flush it), and a live event
// after the page is written once.
#[tokio::test]
async fn every_past_start_streams_at_most_one_page_then_a_summary() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const BACKLOG_N: usize = 500;
    let cursors = publish_n(&daemon, channel, BACKLOG_N).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Everything in the room, in order: each round's live event joins the
    // backlog the next round pages.
    let mut log: Vec<(String, IpcCursor)> = cursors
        .iter()
        .enumerate()
        .map(|(i, c)| (format!("backlog event {i}"), *c))
        .collect();
    // (name, request, events at or before the bookmark, i.e. already read)
    let starts: Vec<(&str, AttachRequest, usize)> = vec![
        (
            "transcript start",
            AttachRequest::new(channel, AttachStart::FromTranscriptStart),
            0,
        ),
        (
            "transcript start + coalesce",
            AttachRequest::new(channel, AttachStart::FromTranscriptStart).with_coalesced_backlog(),
            0,
        ),
        (
            "transcript start + a tail of 200 asked",
            AttachRequest::new(channel, AttachStart::FromTranscriptStart)
                .with_coalesced_backlog()
                .with_backlog_tail(200),
            0,
        ),
        (
            "a bookmark at the first event",
            AttachRequest::new(channel, AttachStart::After(cursors[0])),
            1,
        ),
    ];
    for (name, request, already_read) in starts {
        let newest_page: Vec<String> = log[log.len() - PAGE..]
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        let skipped_expected = (log.len() - already_read - PAGE) as u64;
        let tip = log.last().expect("backlog").1;
        let mut stream = attach_ok(&daemon, request).await;
        assert_eq!(
            read_events(&mut stream, PAGE, name).await,
            newest_page,
            "{name}: the newest page, oldest first"
        );
        match next_frame(&mut stream, &format!("{name}: summary")).await {
            Response::AttachCursorAdvanced {
                skipped,
                advanced_to,
                channel: summary_channel,
            } => {
                assert_eq!(skipped, skipped_expected, "{name}: counts what it left out");
                assert_eq!(advanced_to, tip, "{name}: the tip");
                assert_eq!(
                    summary_channel, None,
                    "{name}: a single-channel summary names no room"
                );
            }
            other => panic!("{name}: expected the summary after the page, got {other:?}"),
        }
        let live_cursor = publish_live_at(&daemon, channel, name.as_bytes()).await;
        let live = read_events(&mut stream, 1, &format!("{name}: live")).await;
        assert_eq!(
            live,
            vec![name.to_string()],
            "{name}: live after the page, once"
        );
        log.push((name.to_string(), live_cursor));
    }
    daemon.stop().await;
}

// what this catches: a bookmark close to the tip gets exactly its unread,
// and no summary (nothing was left out), so a resuming consumer is not told
// about history it already read.
#[tokio::test]
async fn a_bookmark_near_the_tip_gets_exactly_its_unread_and_no_summary() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let cursors = publish_n(&daemon, channel, 20).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut stream = attach_ok(
        &daemon,
        AttachRequest::new(channel, AttachStart::After(cursors[16])),
    )
    .await;
    assert_eq!(
        read_events(&mut stream, 3, "unread").await,
        vec!["backlog event 17", "backlog event 18", "backlog event 19"]
    );
    publish_live(&daemon, channel, b"next").await;
    assert_eq!(
        read_events(&mut stream, 1, "live, no summary between").await,
        vec!["next"]
    );
    daemon.stop().await;
}

// what this catches: a channel SET with a bookmark per room was an unbounded
// forward replay of every room; now each room streams one page and a summary
// that names it, so one socket for N rooms can't be flooded either.
#[tokio::test]
async fn a_set_pages_each_room_and_its_summary_names_the_room() {
    let daemon = start_daemon().await;
    let (a, b) = (RoomId::new(), RoomId::new());
    let ca = publish_n(&daemon, a, 30).await;
    let cb = publish_n(&daemon, b, 30).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut stream = attach_ok(
        &daemon,
        AttachRequest::channel_set(vec![
            ChannelAttach {
                channel: a,
                from: Some(ca[0]),
            },
            ChannelAttach {
                channel: b,
                from: Some(cb[0]),
            },
        ]),
    )
    .await;
    for (room, tip) in [(a, ca[29]), (b, cb[29])] {
        let page = read_events(&mut stream, PAGE, "room page").await;
        assert_eq!(page.first().map(String::as_str), Some("backlog event 20"));
        match next_frame(&mut stream, "room summary").await {
            Response::AttachCursorAdvanced {
                skipped,
                advanced_to,
                channel,
            } => {
                assert_eq!(skipped, 19);
                assert_eq!(advanced_to, tip);
                assert_eq!(channel, Some(room), "a set summary names its room");
            }
            other => panic!("expected the room's summary, got {other:?}"),
        }
    }
    daemon.stop().await;
}

// what this catches: history past the one streamed page is read BACKWARD on
// demand: the newest `limit` events strictly before a cursor, oldest first;
// and `since` with `before` together is refused, not guessed.
#[tokio::test]
async fn inbox_before_pages_history_backward() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let cursors = publish_n(&daemon, channel, 25).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let client = DaemonClient::new(daemon.socket.clone());
    let page = client
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(5),
            kinds: None,
            before: Some(cursors[20]),
        })
        .await
        .expect("inbox before");
    let payloads: Vec<String> = page
        .envelopes
        .into_iter()
        .map(|bytes| {
            let env = airc_wire::decode(bytes.into()).expect("decode");
            String::from_utf8(env.payload.to_vec()).expect("utf8")
        })
        .collect();
    assert_eq!(
        payloads,
        (15..20)
            .map(|i| format!("backlog event {i}"))
            .collect::<Vec<_>>()
    );
    assert!(
        page.has_more,
        "a full backward page says there may be older"
    );
    let both = client
        .inbox(InboxRequest {
            since: Some(cursors[1]),
            channel: Some(channel),
            limit: Some(5),
            kinds: None,
            before: Some(cursors[20]),
        })
        .await;
    assert!(both.is_err(), "since + before is refused: {both:?}");
    daemon.stop().await;
}

/// airc #1416 follow-up (the source-side fix): the live cursor heartbeat
/// is bookkeeping for a consumer that PERSISTS its cursor. A plain live
/// attach never asked for it, so after a live event it must see the
/// Event and then NOTHING — no `AttachCursorAdvanced` — for a window
/// longer than the 1 s throttle.
#[tokio::test]
async fn live_attach_without_the_flag_never_gets_a_cursor_heartbeat() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(AttachRequest::new(channel, AttachStart::Live))
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }
    publish_live(&daemon, channel, b"one").await;
    match next_frame(&mut stream, "live event").await {
        Response::Event { .. } => {}
        other => panic!("expected the live Event first, got {other:?}"),
    }
    match tokio::time::timeout(
        Duration::from_millis(1500),
        read_frame::<_, Response>(&mut stream),
    )
    .await
    {
        Err(_) => { /* quiet = correct: no heartbeat was requested */ }
        Ok(Ok(Some(Response::AttachCursorAdvanced { .. }))) => {
            panic!("a live attach that did not ask for the cursor heartbeat received one")
        }
        Ok(other) => panic!("unexpected frame after the live event: {other:?}"),
    }
    daemon.stop().await;
}

/// The other arm: an attach that ASKS for the heartbeat gets one after a
/// forwarded event (the continuum #261 contract, now opt-in).
#[tokio::test]
async fn live_attach_with_the_flag_gets_a_cursor_heartbeat_after_an_event() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(AttachRequest::new(channel, AttachStart::Live).with_cursor_heartbeat())
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }
    publish_live(&daemon, channel, b"one").await;
    match next_frame(&mut stream, "live event").await {
        Response::Event { .. } => {}
        other => panic!("expected the live Event first, got {other:?}"),
    }
    match next_frame(&mut stream, "cursor heartbeat").await {
        Response::AttachCursorAdvanced { skipped, .. } => assert_eq!(skipped, 0),
        other => panic!("expected the requested cursor heartbeat, got {other:?}"),
    }
    daemon.stop().await;
}
