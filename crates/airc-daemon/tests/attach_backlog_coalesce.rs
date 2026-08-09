//! Card 7d5b6a65 acceptance proof: `attach` with `from_now: true`
//! delivers no backlog, and `attach` with `coalesce_backlog: true`
//! collapses the catch-up phase into ONE
//! `Response::AttachCursorAdvanced` summary frame instead of streaming
//! N historical events.
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
    AttachRequest, AttachStart, DaemonClient, IpcDelivery, IpcKind, IpcTarget, PublishRequest,
    Response,
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
async fn publish_n(daemon: &TestDaemon, channel: RoomId, n: usize) {
    let client = DaemonClient::new(daemon.socket.clone());
    let from_client = uuid::Uuid::new_v4();
    for i in 0..n {
        client
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
    }
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

/// Card 7d5b6a65 acceptance: `coalesce_backlog: true` causes the
/// daemon to emit ONE `AttachCursorAdvanced` summary frame at the
/// catch-up→live seam instead of streaming N historical Event frames.
/// Live events that arrive after the summary still stream
/// event-by-event as before.
#[tokio::test]
async fn attach_coalesce_backlog_emits_one_summary_then_live() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const BACKLOG_N: usize = 30;
    publish_n(&daemon, channel, BACKLOG_N).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(
            AttachRequest::new(channel, AttachStart::FromTranscriptStart).with_coalesced_backlog(),
        )
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }

    // Publish ONE live event AFTER attach; arriving live, it triggers
    // the catch-up summary flush + then its own Event frame.
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
            payload: b"after seam".to_vec(),
            headers: Headers::new(),
        })
        .await
        .expect("publish live");

    // Frame 1: catch-up summary.
    let summary = tokio::time::timeout(
        Duration::from_secs(3),
        read_frame::<_, Response>(&mut stream),
    )
    .await
    .expect("first frame within timeout")
    .expect("frame")
    .expect("Some");
    match summary {
        Response::AttachCursorAdvanced { skipped, .. } => {
            assert_eq!(
                skipped, BACKLOG_N as u64,
                "summary must account for every backlog envelope; \
                 expected {BACKLOG_N}, got {skipped}"
            );
        }
        other => panic!(
            "expected AttachCursorAdvanced as first frame, got {other:?} \
             — coalesce_backlog should collapse backlog into ONE summary"
        ),
    }

    // Frame 2: the live event we published AFTER attach.
    let live = tokio::time::timeout(
        Duration::from_secs(2),
        read_frame::<_, Response>(&mut stream),
    )
    .await
    .expect("live event after summary")
    .expect("frame")
    .expect("Some");
    match live {
        Response::Event { envelope } => {
            let env = airc_wire::decode(envelope.into()).expect("decode");
            assert_eq!(env.payload.to_vec(), b"after seam".to_vec());
        }
        other => panic!("expected Event frame after summary, got {other:?}"),
    }
    daemon.stop().await;
}

/// Card 7d5b6a65 backward-compat acceptance: a client that omits
/// `from_now` and `coalesce_backlog` (the pre-card-7d5b6a65 wire
/// shape) gets the legacy event-by-event replay so audit / replay
/// tooling that needs every historical envelope keeps working.
#[tokio::test]
async fn attach_legacy_shape_still_replays_event_by_event() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const BACKLOG_N: usize = 5;
    publish_n(&daemon, channel, BACKLOG_N).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        // The legacy wire shape: full transcript replay, named explicitly.
        .attach(AttachRequest::new(
            channel,
            AttachStart::FromTranscriptStart,
        ))
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }

    // Collect BACKLOG_N Event frames — legacy event-by-event replay. The
    // cursor HEARTBEAT (continuum #261) may interleave AttachCursorAdvanced
    // frames on ANY attach shape now; clients must tolerate them. The
    // no-skip property they must uphold: an advance NEVER precedes the
    // delivery of the event it points at — a consumer persisting
    // `advanced_to` can only ever resume at-or-before what it has seen.
    let mut events_seen = 0usize;
    let mut advances_seen = 0usize;
    while events_seen < BACKLOG_N {
        let frame = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<_, Response>(&mut stream),
        )
        .await
        .unwrap_or_else(|_| panic!("backlog frame timeout after {events_seen} events"))
        .expect("frame")
        .expect("Some");
        match frame {
            Response::Event { .. } => events_seen += 1,
            Response::AttachCursorAdvanced { skipped, .. } => {
                assert_eq!(
                    skipped, 0,
                    "heartbeat advances report skipped=0 (nothing suppressed)"
                );
                assert!(
                    events_seen > 0,
                    "no-skip guarantee: an advance must never arrive before \
                     the first delivered event"
                );
                advances_seen += 1;
            }
            other => panic!("unexpected frame after {events_seen} events: {other:?}"),
        }
    }
    // The heartbeat is throttled (1/s), so a fast replay yields at least the
    // first-event advance; more are allowed, none required beyond it.
    assert!(
        advances_seen >= 1,
        "cursor heartbeat: at least one advance rides a legacy replay"
    );
    daemon.stop().await;
}

/// Card 7d5b6a65 extension ("one page back"): with `backlog_tail: 2`
/// over 5 backlog events, the seam delivers the LAST 2 as real Event
/// frames, then ONE summary accounting for the 3 older events with the
/// watermark at the last backlog cursor, then the live event.
// what this catches: the seam flush order and the skipped arithmetic —
// a tail written AFTER the summary (or a summary counting delivered
// tail events as skipped) would let a watermark-persisting consumer
// skip events it never received, or double-count history.
#[tokio::test]
async fn backlog_tail_delivers_last_n_then_summary_then_live() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const BACKLOG_N: usize = 5;
    const TAIL: u32 = 2;
    publish_n(&daemon, channel, BACKLOG_N).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(
            AttachRequest::new(channel, AttachStart::FromTranscriptStart)
                .with_coalesced_backlog()
                .with_backlog_tail(TAIL),
        )
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }
    publish_live(&daemon, channel, b"after seam").await;

    // Frames 1..=TAIL: the last TAIL backlog events, oldest first
    // (indices 3 and 4 of the 0-indexed publish loop).
    let mut last_tail_cursor = None;
    for i in (BACKLOG_N - TAIL as usize)..BACKLOG_N {
        match next_frame(&mut stream, &format!("tail event {i}")).await {
            Response::Event { envelope } => {
                let env = airc_wire::decode(envelope.into()).expect("decode tail");
                assert_eq!(
                    env.payload.to_vec(),
                    format!("backlog event {i}").into_bytes(),
                    "tail must be the MOST RECENT backlog events, in order"
                );
                last_tail_cursor = Some(env.cursor());
            }
            other => panic!("expected tail Event frame for backlog event {i}, got {other:?}"),
        }
    }

    // Next frame: the summary. skipped counts ONLY the coalesced
    // (undelivered) events; advanced_to is the last backlog cursor,
    // which equals the last tail event's cursor (≥ every tail cursor —
    // the no-skip watermark invariant).
    match next_frame(&mut stream, "seam summary").await {
        Response::AttachCursorAdvanced {
            skipped,
            advanced_to,
        } => {
            assert_eq!(
                skipped,
                (BACKLOG_N - TAIL as usize) as u64,
                "summary must count only events NOT delivered in the tail"
            );
            let last = last_tail_cursor.expect("saw tail events");
            assert_eq!(advanced_to.epoch, last.seq.epoch);
            assert_eq!(advanced_to.counter, last.seq.counter);
            assert_eq!(advanced_to.event_id, last.event_id);
        }
        other => panic!("expected AttachCursorAdvanced after the tail, got {other:?}"),
    }

    // Final frame: the live event that triggered the seam.
    match next_frame(&mut stream, "live event").await {
        Response::Event { envelope } => {
            let env = airc_wire::decode(envelope.into()).expect("decode live");
            assert_eq!(env.payload.to_vec(), b"after seam".to_vec());
        }
        other => panic!("expected live Event frame after summary, got {other:?}"),
    }
    daemon.stop().await;
}

/// Card 7d5b6a65 extension regression: `backlog_tail: 0` behaves
/// exactly like plain `coalesce_backlog` — one summary counting the
/// whole backlog, then the live event, no tail frames.
// what this catches: the tail plumbing changing behavior for callers
// that did not opt in — 0 (and None, covered by
// attach_coalesce_backlog_emits_one_summary_then_live above) must stay
// byte-identical to the pre-extension coalesce.
#[tokio::test]
async fn backlog_tail_zero_matches_plain_coalesce() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const BACKLOG_N: usize = 5;
    publish_n(&daemon, channel, BACKLOG_N).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(
            AttachRequest::new(channel, AttachStart::FromTranscriptStart)
                .with_coalesced_backlog()
                .with_backlog_tail(0),
        )
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }
    publish_live(&daemon, channel, b"after seam").await;

    match next_frame(&mut stream, "coalesce summary").await {
        Response::AttachCursorAdvanced { skipped, .. } => {
            assert_eq!(
                skipped, BACKLOG_N as u64,
                "tail_cap=0 summary must account for EVERY backlog envelope"
            );
        }
        other => panic!("expected AttachCursorAdvanced first (no tail frames), got {other:?}"),
    }
    match next_frame(&mut stream, "live event").await {
        Response::Event { envelope } => {
            let env = airc_wire::decode(envelope.into()).expect("decode live");
            assert_eq!(env.payload.to_vec(), b"after seam".to_vec());
        }
        other => panic!("expected live Event after summary, got {other:?}"),
    }
    daemon.stop().await;
}

/// Card 7d5b6a65 extension: when the whole backlog fits inside the
/// tail (2 events, tail_cap 5), BOTH are delivered as Event frames and
/// the summary still arrives with skipped=0 carrying the watermark.
// what this catches: dropping the summary when the post-tail remainder
// is 0 — the client needs the watermark frame to persist its cursor
// even when nothing was actually coalesced away.
#[tokio::test]
async fn backlog_smaller_than_tail_delivers_all_plus_watermark() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const BACKLOG_N: usize = 2;
    publish_n(&daemon, channel, BACKLOG_N).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(daemon.socket.clone());
    let mut stream = client
        .attach(
            AttachRequest::new(channel, AttachStart::FromTranscriptStart)
                .with_coalesced_backlog()
                .with_backlog_tail(5),
        )
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }
    publish_live(&daemon, channel, b"after seam").await;

    let mut last_tail_cursor = None;
    for i in 0..BACKLOG_N {
        match next_frame(&mut stream, &format!("tail event {i}")).await {
            Response::Event { envelope } => {
                let env = airc_wire::decode(envelope.into()).expect("decode tail");
                assert_eq!(
                    env.payload.to_vec(),
                    format!("backlog event {i}").into_bytes()
                );
                last_tail_cursor = Some(env.cursor());
            }
            other => panic!("expected tail Event frame {i}, got {other:?}"),
        }
    }
    match next_frame(&mut stream, "watermark summary").await {
        Response::AttachCursorAdvanced {
            skipped,
            advanced_to,
        } => {
            assert_eq!(skipped, 0, "everything was delivered — nothing skipped");
            let last = last_tail_cursor.expect("saw tail events");
            assert_eq!(advanced_to.epoch, last.seq.epoch);
            assert_eq!(advanced_to.counter, last.seq.counter);
            assert_eq!(advanced_to.event_id, last.event_id);
        }
        other => panic!("expected skipped=0 watermark summary, got {other:?}"),
    }
    match next_frame(&mut stream, "live event").await {
        Response::Event { envelope } => {
            let env = airc_wire::decode(envelope.into()).expect("decode live");
            assert_eq!(env.payload.to_vec(), b"after seam".to_vec());
        }
        other => panic!("expected live Event after watermark, got {other:?}"),
    }
    daemon.stop().await;
}
