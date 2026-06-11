//! Card 28c89065 — invariant (c) of the store-as-arbiter
//! peer-coordination contract:
//!
//! > Card state transitions propagate to ALL attached peers via the
//! > store.
//!
//! The other contract invariants are proven against the projection in
//! `airc-work` (card 5d65aec2): "one-winner under concurrent claims",
//! "lease expiry permits reclaim", "concurrent creates with distinct
//! ids don't collide". Those are pure replay properties of
//! `WorkBoardProjection`.
//!
//! Invariant (c) is different: it lives at the cross-IPC delivery
//! layer, not projection. The contract only holds end-to-end if the
//! daemon's router actually fans every published `WorkEvent` out to
//! every attached subscriber on the same room — byte-identical, in
//! the same total order — and every subscriber decodes back to the
//! same `WorkEvent` and projects to the same final board state.
//!
//! That's what this file pins. One live daemon over a real ORM, two
//! observer peers attached, one publisher walking a card through its
//! full lifecycle (create → claim → InProgress → Closed → release)
//! via exactly the codec `airc-lib::work::publish_work_event` uses.
//! If `encode_work_event`, the daemon router, or the wire codec ever
//! drift apart, this test fails.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_bus::envelope::Envelope;
use airc_core::{Body, PeerId, RoomId};
use airc_daemon::{run, DaemonRuntimeInfo, DaemonState};
use airc_ipc::codec::read_frame;
use airc_ipc::{
    AttachRequest, DaemonClient, IpcDelivery, IpcKind, IpcTarget, PublishRequest, Response,
};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::{EventStore, InMemoryEventStore};
use airc_work::{
    decode_work_event, encode_work_event, CardCreated, CardState, CardStateChanged, ClaimId,
    ClaimReleased, Priority, RepoId, WorkBoardProjection, WorkCardClaimed, WorkCardId, WorkEvent,
};
use tokio::sync::Barrier;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Live daemon harness — mirrors owner_core_proof.rs so the proof talks to the
// same DaemonState construction the production binary uses.
// ---------------------------------------------------------------------------

struct TestDaemon {
    socket: PathBuf,
    handle: JoinHandle<()>,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    // Short /tmp path to stay under macOS SUN_LEN (104 bytes).
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-wepp-{}-{n}.sock", std::process::id()))
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
        _home: home,
    }
}

impl TestDaemon {
    async fn stop(self) {
        let _ = DaemonClient::new(self.socket.clone()).stop().await;
        let _ = tokio::time::timeout(Duration::from_secs(3), self.handle).await;
    }
}

/// Attach, confirm the subscribe-before-ack `Ok`, sync on `ready`, then
/// collect `want` envelopes from the live stream.
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
                out.push(airc_wire::decode(envelope.into()).expect("decode wire envelope"));
            }
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

/// Build a `PublishRequest` exactly the way `airc-lib::work::publish_work_event`
/// does: `encode_work_event` → routable headers + JSON `Body`,
/// `kind = Event`, `delivery = Durable`. If the production codec ever
/// changes shape, the wire bytes this test exchanges change with it.
fn publish_work_event_request(
    channel: RoomId,
    from_peer: uuid::Uuid,
    from_client: uuid::Uuid,
    event: &WorkEvent,
) -> PublishRequest {
    let (headers, body) = encode_work_event(event).expect("encode work event");
    PublishRequest {
        channel: channel.as_uuid(),
        from_peer,
        from_client,
        kind: IpcKind::Event,
        delivery: IpcDelivery::Durable,
        target: IpcTarget::All,
        correlation_id: None,
        coalesce_key: None,
        payload: body.to_payload(),
        headers,
    }
}

fn participant(peer: u128, client: u128) -> (uuid::Uuid, uuid::Uuid) {
    (uuid::Uuid::from_u128(peer), uuid::Uuid::from_u128(client))
}

fn repo() -> RepoId {
    RepoId::new("CambrianTech/airc").expect("repo id")
}

// ---------------------------------------------------------------------------
// The proof.
// ---------------------------------------------------------------------------

/// Card 28c89065, invariant (c): card-state transitions propagate to
/// **all** attached peers via the store.
///
/// One daemon, one room, two observer peers attached and acked
/// (subscribe-before-ack — no transition can sneak in before the
/// subscription is live). The publisher then walks a single card
/// through the canonical lifecycle:
///
/// ```text
///   card_created          (Open)
///   card_claimed          (Claimed)
///   card_state_changed → InProgress
///   card_state_changed → Closed
///   claim_released
/// ```
///
/// The proof has four parts:
///
/// 1. **No drop.** Both observers receive every transition (no fan-out
///    bug silently masks a transition).
/// 2. **Same order.** Both observers receive transitions in the same
///    total order — the daemon's router is the single ordering point
///    that the store-as-arbiter contract relies on.
/// 3. **Byte-identical decode.** Each observer decodes each envelope
///    back to the SAME `WorkEvent` the publisher authored. This pins
///    the wire-codec round-trip, which is the OTHER half of "no
///    silent clobber" (the projection-layer half is card 5d65aec2).
/// 4. **Projection convergence.** Replaying the observed sequence
///    through `WorkBoardProjection` yields the same final card state
///    on both sides. Together with the projection-layer invariants
///    (a)/(b)/(d), this closes the loop: what one peer sees as
///    "card 28c89065 is Closed, claim released" every other attached
///    peer sees too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn card_state_transitions_propagate_to_all_attached_peers() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();

    // Two observer peers — the "all attached peers" half of (c).
    let ready = Arc::new(Barrier::new(3));
    let obs_a = tokio::spawn(collect_envelopes(
        daemon.socket.clone(),
        channel,
        5,
        ready.clone(),
    ));
    let obs_b = tokio::spawn(collect_envelopes(
        daemon.socket.clone(),
        channel,
        5,
        ready.clone(),
    ));
    ready.wait().await;

    // Author the lifecycle transcript.
    let card_id = WorkCardId::from_u128(0x28C8_9065);
    let claim_id = ClaimId::from_u128(0xC1A1_C1A1);
    let owner = PeerId::from_u128(0xA11CE);

    let transcript: Vec<WorkEvent> = vec![
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "invariant (c) — propagation".into(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 1,
            reviews: None,
        origin: None,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id,
            owner,
            ttl_ms: 60_000,
            claimed_at_ms: 2,
        }),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id,
            state: CardState::InProgress,
            changed_by: owner,
            changed_at_ms: 3,
        }),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id,
            state: CardState::Closed,
            changed_by: owner,
            changed_at_ms: 4,
        }),
        WorkEvent::ClaimReleased(ClaimReleased {
            card_id,
            claim_id,
            owner,
            reason: None,
            released_at_ms: 5,
        }),
    ];

    let (from_peer, from_client) = participant(0xA11CE, 0x7AB);
    let publisher = DaemonClient::new(daemon.socket.clone());
    for event in &transcript {
        publisher
            .publish(publish_work_event_request(
                channel,
                from_peer,
                from_client,
                event,
            ))
            .await
            .expect("publish work event");
    }

    let envs_a = tokio::time::timeout(Duration::from_secs(15), obs_a)
        .await
        .expect("observer A did not finish")
        .expect("observer A join");
    let envs_b = tokio::time::timeout(Duration::from_secs(15), obs_b)
        .await
        .expect("observer B did not finish")
        .expect("observer B join");

    // (1) No drop.
    assert_eq!(
        envs_a.len(),
        transcript.len(),
        "observer A saw a partial transcript: {}/{}",
        envs_a.len(),
        transcript.len()
    );
    assert_eq!(
        envs_b.len(),
        transcript.len(),
        "observer B saw a partial transcript: {}/{}",
        envs_b.len(),
        transcript.len()
    );

    // (2,3) Same order, byte-identical decoded events on both sides.
    let decode = |envs: Vec<Envelope>| -> Vec<WorkEvent> {
        envs.into_iter()
            .map(|env| {
                let body = Body::from_payload(&env.payload).expect("payload decodes as Body");
                decode_work_event(&env.headers, Some(&body)).expect("decode work event")
            })
            .collect()
    };
    let decoded_a = decode(envs_a);
    let decoded_b = decode(envs_b);
    assert_eq!(
        decoded_a, transcript,
        "observer A saw a different transcript than the publisher authored"
    );
    assert_eq!(
        decoded_b, transcript,
        "observer B saw a different transcript than the publisher authored"
    );

    // (4) Projection convergence — both observers' projections agree
    //     on the card's final state. This is the user-visible half of
    //     "the board you see matches the board everyone else sees".
    let project = |events: Vec<WorkEvent>| -> WorkBoardProjection {
        WorkBoardProjection::replay_window(events).expect("replay projects")
    };
    let proj_a = project(decoded_a);
    let proj_b = project(decoded_b);
    let card_a = proj_a.card(card_id).expect("observer A projected the card");
    let card_b = proj_b.card(card_id).expect("observer B projected the card");
    assert_eq!(card_a.state, CardState::Closed);
    assert_eq!(card_b.state, CardState::Closed);
    assert_eq!(card_a.claim_id, None, "observer A: claim released");
    assert_eq!(card_b.claim_id, None, "observer B: claim released");

    daemon.stop().await;
}
