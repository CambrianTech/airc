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

use airc_core::{HeaderFilter, Headers, PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::codec::read_frame;
use airc_ipc::{
    AttachRequest, DaemonClient, IpcDelivery, IpcKind, IpcTarget, PublishRequest, Response,
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
        .attach(AttachRequest {
            channel: Some(channel),
            from: None,
            from_now: true,
            coalesce_backlog: false,
            kinds: None,
            delivery: None,
            headers: HeaderFilter::default(),
        })
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
        .attach(AttachRequest {
            channel: Some(channel),
            from: None,
            from_now: false,
            coalesce_backlog: true,
            kinds: None,
            delivery: None,
            headers: HeaderFilter::default(),
        })
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
        .attach(AttachRequest {
            channel: Some(channel),
            from: None,
            // Both new fields default to false — the legacy wire shape.
            ..Default::default()
        })
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }

    // Collect BACKLOG_N Event frames — legacy event-by-event replay.
    for i in 0..BACKLOG_N {
        let frame = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<_, Response>(&mut stream),
        )
        .await
        .unwrap_or_else(|_| panic!("backlog event {i} timeout"))
        .expect("frame")
        .expect("Some");
        match frame {
            Response::Event { .. } => { /* expected */ }
            Response::AttachCursorAdvanced { .. } => panic!(
                "legacy shape (no coalesce_backlog) must NOT emit \
                 AttachCursorAdvanced; got it at event index {i}"
            ),
            other => panic!("unexpected frame at index {i}: {other:?}"),
        }
    }
    daemon.stop().await;
}
