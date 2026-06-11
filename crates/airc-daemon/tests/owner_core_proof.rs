//! Owner-core daemon acceptance proofs (§11.1 against the LIVE daemon,
//! not `airc-bus` in isolation).
//!
//! These prove the substrate can carry what Continuum / the grid / games
//! actually push at it, same-machine:
//!   - **14 personas in one room** all converge on every message
//!     (concurrent sessions, in-memory fan-out).
//!   - **WebRTC / media / inference raw bytes** ride `StreamChunk` as an
//!     OPAQUE payload — byte-identical end to end (no serde/JSON mangling)
//!     and **never persisted** (off the ORM, the low-CPU live path).
//!   - **Durable** chat replays in total order via `inbox`, cursor-paged.
//!   - The daemon can be **hammered** (high volume) without stalling or
//!     dropping.
//!
//! The test model IS the production model: a real `DaemonState` over a
//! real SQLite ORM on a Unix socket, driven by the real `DaemonClient`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use airc_bus::envelope::{DeliveryClass, Envelope, Kind};
use airc_core::{HeaderFilter, Headers, PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::codec::read_frame;
use airc_ipc::{
    AttachRequest, DaemonClient, InboxRequest, IpcDelivery, IpcKind, IpcTarget, PublishRequest,
    Response, SendRequest,
};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use tokio::sync::Barrier;
use tokio::task::JoinHandle;

/// A live daemon on a Unix socket, owning a real router + SQLite ORM.
struct TestDaemon {
    socket: PathBuf,
    handle: JoinHandle<()>,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    // Short /tmp path keeps us well under macOS SUN_LEN (104 bytes).
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-ocp-{}-{n}.sock", std::process::id()))
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
    // Wait for the listener to bind (the socket file appears).
    for _ in 0..200 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    TestDaemon {
        socket,
        handle,
        _home: home,
    }
}

impl TestDaemon {
    async fn stop(self) {
        let _ = DaemonClient::new(self.socket.clone()).stop().await;
        let _ = tokio::time::timeout(Duration::from_secs(3), self.handle).await;
    }
}

/// Attach to `channel`, confirm the `Ok` ack (which — subscribe-before-ack
/// — means the subscription is already live), sync on `ready`, then
/// collect `want` event payloads (decoded from airc-wire bytes).
async fn persona_collect(
    socket: PathBuf,
    channel: RoomId,
    want: usize,
    ready: Arc<Barrier>,
) -> Vec<Vec<u8>> {
    let client = DaemonClient::new(socket);
    let mut stream = client
        .attach(AttachRequest::live(channel))
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack from attach, got {other:?}"),
    }
    ready.wait().await;

    let mut out = Vec::with_capacity(want);
    while out.len() < want {
        match tokio::time::timeout(
            Duration::from_secs(20),
            read_frame::<_, Response>(&mut stream),
        )
        .await
        {
            Ok(Ok(Some(Response::Event { envelope }))) => {
                let env = airc_wire::decode(envelope.into()).expect("decode airc-wire event");
                out.push(env.payload.to_vec());
            }
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

/// Like [`persona_collect`] but returns fully-decoded envelopes so a
/// test can assert per-delivery-class behaviour (the Continuum mixed-room
/// proof).
async fn collect_envelopes(
    socket: PathBuf,
    channel: RoomId,
    want: usize,
    ready: Arc<Barrier>,
) -> Vec<Envelope> {
    let client = DaemonClient::new(socket);
    let mut stream = client
        .attach(AttachRequest::live(channel))
        .await
        .expect("attach");
    match read_frame::<_, Response>(&mut stream).await {
        Ok(Some(Response::Ok)) => {}
        other => panic!("expected Ok ack, got {other:?}"),
    }
    ready.wait().await;

    let mut out = Vec::with_capacity(want);
    while out.len() < want {
        match tokio::time::timeout(
            Duration::from_secs(20),
            read_frame::<_, Response>(&mut stream),
        )
        .await
        {
            Ok(Ok(Some(Response::Event { envelope }))) => {
                out.push(airc_wire::decode(envelope.into()).expect("decode"));
            }
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

/// A stable synthetic participant identity for a test publisher. Every
/// publish now carries the originating participant (the daemon is a
/// broker, not the author), so tests supply one.
fn participant(peer: u128, client: u128) -> (uuid::Uuid, uuid::Uuid) {
    (uuid::Uuid::from_u128(peer), uuid::Uuid::from_u128(client))
}

fn durable_text(channel: RoomId, text: &str) -> PublishRequest {
    let (from_peer, from_client) = participant(0xA11CE, 0x7AB);
    PublishRequest {
        channel: channel.as_uuid(),
        from_peer,
        from_client,
        kind: IpcKind::Message,
        delivery: IpcDelivery::Durable,
        target: IpcTarget::All,
        correlation_id: None,
        coalesce_key: None,
        payload: text.as_bytes().to_vec(),
        headers: Headers::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fourteen_personas_in_one_room_all_converge() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const PERSONAS: usize = 14;
    const MESSAGES: usize = 50;

    let ready = Arc::new(Barrier::new(PERSONAS + 1));
    let mut personas = Vec::new();
    for _ in 0..PERSONAS {
        personas.push(tokio::spawn(persona_collect(
            daemon.socket.clone(),
            channel,
            MESSAGES,
            ready.clone(),
        )));
    }

    // Every persona is attached + acked (subscribed at the live edge)
    // before the first publish — no message can be missed.
    ready.wait().await;
    let publisher = DaemonClient::new(daemon.socket.clone());
    for i in 0..MESSAGES {
        publisher
            .publish(durable_text(channel, &format!("m{i}")))
            .await
            .expect("publish");
    }

    for persona in personas {
        let got = tokio::time::timeout(Duration::from_secs(25), persona)
            .await
            .expect("persona did not finish")
            .expect("persona join");
        assert_eq!(
            got.len(),
            MESSAGES,
            "every one of the 14 personas must see every message"
        );
        for (i, payload) in got.iter().enumerate() {
            assert_eq!(payload, format!("m{i}").as_bytes(), "in-order convergence");
        }
    }
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamchunk_raw_bytes_route_live_byte_identical_and_never_persist() {
    // The WebRTC / media / remote-inference path: raw bytes, opaque, no
    // codec. Prove they arrive byte-for-byte (no serde/JSON mangling) and
    // produce ZERO durable rows (off the ORM — the low-CPU live path).
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let frames: Vec<Vec<u8>> = vec![
        vec![0x00, 0x01, 0x02, 0xfe, 0xff],
        vec![0x42; 4096],
        (0..=255u8).collect(),
    ];

    let ready = Arc::new(Barrier::new(2));
    let collector = tokio::spawn(persona_collect(
        daemon.socket.clone(),
        channel,
        frames.len(),
        ready.clone(),
    ));
    ready.wait().await;

    let publisher = DaemonClient::new(daemon.socket.clone());
    for frame in &frames {
        publisher
            .publish(PublishRequest {
                channel: channel.as_uuid(),
                from_peer: uuid::Uuid::from_u128(0x573EA3),
                from_client: uuid::Uuid::from_u128(0x573EAC),
                kind: IpcKind::Event,
                delivery: IpcDelivery::StreamChunk,
                target: IpcTarget::All,
                correlation_id: None,
                coalesce_key: None,
                payload: frame.clone(), // RAW — no Body, no serde
                headers: Headers::new(),
            })
            .await
            .expect("publish stream chunk");
    }

    let got = tokio::time::timeout(Duration::from_secs(15), collector)
        .await
        .expect("collector did not finish")
        .expect("collector join");
    assert_eq!(
        got, frames,
        "raw stream bytes arrive byte-identical — opaque payload, no serialization tax"
    );

    // Off the ORM: a StreamChunk is never durable, so inbox is empty.
    let inbox = publisher
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(100),
        })
        .await
        .expect("inbox");
    assert!(
        inbox.envelopes.is_empty(),
        "StreamChunk must never reach the durable tier"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn durable_publishes_replay_via_inbox_in_order_and_page_by_cursor() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let publisher = DaemonClient::new(daemon.socket.clone());
    const N: usize = 10;
    for i in 0..N {
        publisher
            .publish(durable_text(channel, &format!("d{i}")))
            .await
            .expect("publish");
    }

    let inbox = publisher
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(100),
        })
        .await
        .expect("inbox");
    assert_eq!(inbox.envelopes.len(), N, "all durable events replay");
    for (i, bytes) in inbox.envelopes.iter().enumerate() {
        let env = airc_wire::decode(bytes.clone().into()).expect("decode");
        assert_eq!(env.payload.as_ref(), format!("d{i}").as_bytes());
    }

    // Resuming strictly after the newest cursor yields nothing.
    let after = publisher
        .inbox(InboxRequest {
            since: inbox.newest,
            channel: Some(channel),
            limit: Some(100),
        })
        .await
        .expect("inbox after cursor");
    assert!(
        after.envelopes.is_empty(),
        "no events after the newest cursor (consume-once)"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_can_be_hammered_with_volume_without_dropping() {
    // Throughput: one persona must receive a large burst, in order,
    // promptly. Proves the in-memory fan-out path sustains volume (no
    // poll floor, no per-frame re-verify).
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const BURST: usize = 2_000;

    let ready = Arc::new(Barrier::new(2));
    let collector = tokio::spawn(persona_collect(
        daemon.socket.clone(),
        channel,
        BURST,
        ready.clone(),
    ));
    ready.wait().await;

    let started = Instant::now();
    let publisher = DaemonClient::new(daemon.socket.clone());
    for i in 0..BURST {
        publisher
            .publish(PublishRequest {
                channel: channel.as_uuid(),
                from_peer: uuid::Uuid::from_u128(0x4A33E2),
                from_client: uuid::Uuid::from_u128(0x4A33EC),
                kind: IpcKind::Event,
                delivery: IpcDelivery::StreamChunk, // live path, off-ORM
                target: IpcTarget::All,
                correlation_id: None,
                coalesce_key: None,
                payload: (i as u32).to_be_bytes().to_vec(),
                headers: Headers::new(),
            })
            .await
            .expect("publish");
    }

    let got = tokio::time::timeout(Duration::from_secs(30), collector)
        .await
        .expect("collector did not finish under load")
        .expect("collector join");
    let elapsed = started.elapsed();
    assert_eq!(got.len(), BURST, "every event in the burst is delivered");
    for (i, payload) in got.iter().enumerate() {
        assert_eq!(payload.as_slice(), &(i as u32).to_be_bytes(), "in order");
    }
    // Generous bound — this is a "doesn't stall / scales" gate, not a
    // micro-benchmark. CI machines vary; 2000 round-trips in 30s is a
    // floor any healthy in-memory path clears with room to spare.
    assert!(
        elapsed < Duration::from_secs(30),
        "burst delivered in {elapsed:?}"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn continuum_webrtc_room_mixed_traffic_only_chat_persists() {
    // The Continuum WebRTC room, same-machine: 14 personas in one room,
    // each pushing the three traffic classes that room actually carries —
    //   * Durable      → chat (must survive, replay on room open)
    //   * StreamChunk   → audio/video frames (raw, drop-safe, off-ORM)
    //   * EphemeralLatest → avatar-state/presence (latest-wins per persona)
    // Proof: a viewer sees the whole mix live, but the ORM only ever holds
    // the chat — the 21MB/s media + 30fps pose firehose never touches
    // SQLite. That's what keeps the substrate cheap under a real room.
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    const PERSONAS: usize = 14;
    const CHAT: usize = 1;
    const MEDIA: usize = 5;
    const POSE: usize = 3;
    let per_persona = CHAT + MEDIA + POSE;
    let total = PERSONAS * per_persona;

    let ready = Arc::new(Barrier::new(2));
    let viewer = tokio::spawn(collect_envelopes(
        daemon.socket.clone(),
        channel,
        total,
        ready.clone(),
    ));
    ready.wait().await;

    let publisher = DaemonClient::new(daemon.socket.clone());
    for persona in 0..PERSONAS {
        // Durable chat.
        publisher
            .publish(durable_text(
                channel,
                &format!("chat from persona {persona}"),
            ))
            .await
            .expect("chat");
        // StreamChunk media frames — raw bytes, off-ORM.
        for f in 0..MEDIA {
            publisher
                .publish(PublishRequest {
                    channel: channel.as_uuid(),
                    from_peer: uuid::Uuid::from_u128(0x50600000 + persona as u128),
                    from_client: uuid::Uuid::from_u128(0x5060C000 + persona as u128),
                    kind: IpcKind::Event,
                    delivery: IpcDelivery::StreamChunk,
                    target: IpcTarget::All,
                    correlation_id: None,
                    coalesce_key: None,
                    payload: vec![persona as u8, f as u8, 0xAA, 0xBB],
                    headers: Headers::new(),
                })
                .await
                .expect("media");
        }
        // EphemeralLatest avatar-state — coalesced per persona, off-ORM.
        for p in 0..POSE {
            publisher
                .publish(PublishRequest {
                    channel: channel.as_uuid(),
                    from_peer: uuid::Uuid::from_u128(0x50600000 + persona as u128),
                    from_client: uuid::Uuid::from_u128(0x5060C000 + persona as u128),
                    kind: IpcKind::Event,
                    delivery: IpcDelivery::EphemeralLatest,
                    target: IpcTarget::All,
                    correlation_id: None,
                    coalesce_key: Some(format!("avatar:{persona}")),
                    payload: vec![persona as u8, p as u8],
                    headers: Headers::new(),
                })
                .await
                .expect("pose");
        }
    }

    let got = tokio::time::timeout(Duration::from_secs(25), viewer)
        .await
        .expect("viewer did not finish")
        .expect("viewer join");
    assert_eq!(got.len(), total, "viewer sees the entire live mix");

    let durable = got
        .iter()
        .filter(|e| e.delivery == DeliveryClass::Durable)
        .count();
    let stream = got
        .iter()
        .filter(|e| e.delivery == DeliveryClass::StreamChunk)
        .count();
    let ephemeral = got
        .iter()
        .filter(|e| e.delivery == DeliveryClass::EphemeralLatest)
        .count();
    assert_eq!(durable, PERSONAS * CHAT, "all chat delivered");
    assert_eq!(stream, PERSONAS * MEDIA, "all media delivered live");
    assert_eq!(
        ephemeral,
        PERSONAS * POSE,
        "all pose updates delivered live"
    );

    // The ORM only holds the chat — media + pose never persisted.
    let inbox = publisher
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(1000),
        })
        .await
        .expect("inbox");
    assert_eq!(
        inbox.envelopes.len(),
        PERSONAS * CHAT,
        "ORM holds ONLY durable chat — the media/pose firehose never hit SQLite"
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_send_is_durable_and_text_round_trips() {
    // `Send` is the one daemon-authored codec (chat text → canonical
    // JSON Body). Confirm it persists and the text survives the round
    // trip through the opaque payload.
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let client = DaemonClient::new(daemon.socket.clone());
    client
        .send(SendRequest {
            channel: channel.as_uuid(),
            from_peer: uuid::Uuid::from_u128(0xC4A7),
            from_client: uuid::Uuid::from_u128(0xC4A8),
            text: "hello over the owner-core".to_string(),
            headers: Headers::new(),
        })
        .await
        .expect("send");

    let inbox = client
        .inbox(InboxRequest {
            since: None,
            channel: Some(channel),
            limit: Some(10),
        })
        .await
        .expect("inbox");
    assert_eq!(inbox.envelopes.len(), 1, "chat send is durable");
    let env = airc_wire::decode(inbox.envelopes[0].clone().into()).expect("decode");
    let body = airc_core::Body::from_payload(&env.payload).expect("body decodes");
    assert_eq!(body.as_text(), Some("hello over the owner-core"));
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_response_rpc_correlates_across_kind_filtered_sessions() {
    // The grid / foundry / Hermes RPC pattern: a requester issues a
    // `Command` with a correlation id; a worker subscribed to ONLY
    // `Command` (router-side kind filter) handles it and replies a
    // `CommandResult` correlated by the same id; the requester subscribed
    // to ONLY `CommandResult` gets the answer. Proves: correlation_id
    // round-trips, the full Kind vocabulary publishes, and router-side
    // kind filters cleanly separate the request/response legs.
    let daemon = start_daemon().await;
    let channel = RoomId::new();
    let corr = uuid::Uuid::new_v4();
    let ready = Arc::new(Barrier::new(3));

    // Worker: only Commands. Reads one, replies a correlated result.
    let worker_socket = daemon.socket.clone();
    let worker_ready = ready.clone();
    let worker = tokio::spawn(async move {
        let client = DaemonClient::new(worker_socket);
        let mut stream = client
            .attach(AttachRequest::live(channel).with_kinds(vec![IpcKind::Command]))
            .await
            .expect("worker attach");
        assert!(matches!(
            read_frame::<_, Response>(&mut stream).await,
            Ok(Some(Response::Ok))
        ));
        worker_ready.wait().await;
        let cmd = loop {
            if let Ok(Ok(Some(Response::Event { envelope }))) = tokio::time::timeout(
                Duration::from_secs(10),
                read_frame::<_, Response>(&mut stream),
            )
            .await
            {
                break airc_wire::decode(envelope.into()).expect("decode command");
            }
        };
        assert_eq!(cmd.kind, Kind::Command, "worker only sees Commands");
        assert_eq!(
            cmd.correlation_id,
            Some(corr),
            "command carries correlation"
        );
        client
            .publish(PublishRequest {
                channel: channel.as_uuid(),
                from_peer: uuid::Uuid::from_u128(0x5E0001),
                from_client: uuid::Uuid::from_u128(0x5E0002),
                kind: IpcKind::CommandResult,
                delivery: IpcDelivery::RequestResponse,
                target: IpcTarget::Reply(corr),
                correlation_id: Some(corr),
                coalesce_key: None,
                payload: b"result-42".to_vec(),
                headers: Headers::new(),
            })
            .await
            .expect("worker reply");
        cmd.payload.to_vec()
    });

    // Requester: only CommandResults. Reads the one correlated answer.
    let req_socket = daemon.socket.clone();
    let req_ready = ready.clone();
    let requester = tokio::spawn(async move {
        let client = DaemonClient::new(req_socket);
        let mut stream = client
            .attach(AttachRequest::live(channel).with_kinds(vec![IpcKind::CommandResult]))
            .await
            .expect("requester attach");
        assert!(matches!(
            read_frame::<_, Response>(&mut stream).await,
            Ok(Some(Response::Ok))
        ));
        req_ready.wait().await;
        let result = loop {
            if let Ok(Ok(Some(Response::Event { envelope }))) = tokio::time::timeout(
                Duration::from_secs(10),
                read_frame::<_, Response>(&mut stream),
            )
            .await
            {
                break airc_wire::decode(envelope.into()).expect("decode result");
            }
        };
        assert_eq!(
            result.kind,
            Kind::CommandResult,
            "requester only sees results"
        );
        assert_eq!(result.correlation_id, Some(corr), "result pairs to request");
        result.payload.to_vec()
    });

    ready.wait().await;
    DaemonClient::new(daemon.socket.clone())
        .publish(PublishRequest {
            channel: channel.as_uuid(),
            from_peer: uuid::Uuid::from_u128(0xE9035),
            from_client: uuid::Uuid::from_u128(0xE9036),
            kind: IpcKind::Command,
            delivery: IpcDelivery::RequestResponse,
            target: IpcTarget::All,
            correlation_id: Some(corr),
            coalesce_key: None,
            payload: b"infer-this".to_vec(),
            headers: Headers::new(),
        })
        .await
        .expect("publish command");

    let cmd_payload = tokio::time::timeout(Duration::from_secs(15), worker)
        .await
        .expect("worker finished")
        .expect("worker join");
    assert_eq!(cmd_payload, b"infer-this");
    let result_payload = tokio::time::timeout(Duration::from_secs(15), requester)
        .await
        .expect("requester finished")
        .expect("requester join");
    assert_eq!(result_payload, b"result-42");
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attach_header_filter_scopes_subscription_router_side() {
    // Continuum / OpenClaw scope subscriptions by `forge.*` headers. The
    // filter runs ROUTER-SIDE — the daemon delivers only matching events,
    // never fanning out the rest. Publish a non-matching event FIRST: if
    // the filter leaked, the collector would receive it.
    let daemon = start_daemon().await;
    let channel = RoomId::new();

    let mut alpha = Headers::new();
    alpha.insert("forge.room".to_string(), "alpha".to_string());
    let mut beta = Headers::new();
    beta.insert("forge.room".to_string(), "beta".to_string());

    let ready = Arc::new(Barrier::new(2));
    let socket = daemon.socket.clone();
    let r = ready.clone();
    let collector = tokio::spawn(async move {
        let client = DaemonClient::new(socket);
        let mut stream = client
            .attach(
                AttachRequest::live(channel).with_headers(HeaderFilter::Exact {
                    key: "forge.room".to_string(),
                    value: "alpha".to_string(),
                }),
            )
            .await
            .expect("attach");
        assert!(matches!(
            read_frame::<_, Response>(&mut stream).await,
            Ok(Some(Response::Ok))
        ));
        r.wait().await;
        let mut got = Vec::new();
        while got.len() < 2 {
            match tokio::time::timeout(
                Duration::from_secs(10),
                read_frame::<_, Response>(&mut stream),
            )
            .await
            {
                Ok(Ok(Some(Response::Event { envelope }))) => {
                    got.push(airc_wire::decode(envelope.into()).expect("decode"));
                }
                Ok(Ok(Some(_))) => {}
                _ => break,
            }
        }
        got
    });
    ready.wait().await;

    let publisher = DaemonClient::new(daemon.socket.clone());
    let publish = |kind_payload: &'static [u8], headers: Headers| {
        let publisher = &publisher;
        async move {
            publisher
                .publish(PublishRequest {
                    channel: channel.as_uuid(),
                    from_peer: uuid::Uuid::from_u128(0xF117E2),
                    from_client: uuid::Uuid::from_u128(0xF117E3),
                    kind: IpcKind::Event,
                    delivery: IpcDelivery::Durable,
                    target: IpcTarget::All,
                    correlation_id: None,
                    coalesce_key: None,
                    payload: kind_payload.to_vec(),
                    headers,
                })
                .await
                .expect("publish");
        }
    };
    // Non-matching first — must be filtered out router-side.
    publish(b"beta-1", beta.clone()).await;
    publish(b"alpha-1", alpha.clone()).await;
    publish(b"alpha-2", alpha.clone()).await;

    let got = tokio::time::timeout(Duration::from_secs(12), collector)
        .await
        .expect("collector finished")
        .expect("collector join");
    assert_eq!(got.len(), 2, "only the two alpha events match");
    for env in &got {
        assert_eq!(
            env.headers.get("forge.room").map(String::as_str),
            Some("alpha"),
            "router-side header filter excluded non-alpha events"
        );
    }
    daemon.stop().await;
}
