//! Card 75b54d0a — flywheel continuity under agent/machine churn:
//!
//! > Work must outlive any participant.
//!
//! Per AGENTS.md §3 ("Lease + heartbeat keep the flywheel alive
//! across churn"), the contract is: a claim expires after its
//! `ttl_ms`; if the holder stops heartbeating (process died, machine
//! offline, agent context lost), the lease decays and a different
//! peer can reclaim the card via a new `CardClaimed` event whose
//! `claimed_at_ms` is past the prior expiry. Same-card concurrency
//! is resolved at projection by first-write-wins on the active claim
//! — *while the lease is live*. Once the lease has decayed, the
//! winner is whichever peer claims next. The store is the arbiter,
//! and it never asks the dead holder for permission.
//!
//! That's the property this file pins, end-to-end, through the live
//! daemon's IPC router. One daemon, two attached observer peers, a
//! single card carrying the canonical churn transcript:
//!
//! ```text
//!   created  by Alice  @ t=1
//!   claimed  by Alice  @ t=2,  ttl=100        → expires t=102
//!   (Alice goes dark; no heartbeat)
//!   claimed  by Bob    @ t=200, ttl=100       → reclaim, expires t=300
//!   state    InProgress by Bob @ t=201
//!   state    Closed    by Bob @ t=300
//!   release  by Bob    @ t=301
//! ```
//!
//! Proof points:
//!
//! 1. Both observers see every event in order.
//! 2. Replaying the transcript through `WorkBoardProjection.stale_claims`
//!    at `t=200` flags Alice's claim as reclaim-eligible on both sides
//!    (lease expiry is observable cross-peer, not just to the holder).
//! 3. Bob's reclaim takes ownership on both observers' projections —
//!    the projection does NOT silently drop the reclaim as a duplicate
//!    active claim (the dead lease is no longer "active").
//! 4. The final card state on both sides is `Closed`, owned by nobody
//!    (release applied), with no orphan claim hanging on Alice. Work
//!    survived Alice; the substrate did not require Alice to come back.
//!
//! Together with `work_event_propagation_proof.rs` (card 28c89065,
//! invariant (c): state transitions propagate to all attached peers),
//! this closes the autonomous-team substrate contract: peers come and
//! go, claims expire, work continues — without any single peer being
//! a single point of failure.

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
// Live daemon harness (mirrors owner_core_proof.rs +
// work_event_propagation_proof.rs).
// ---------------------------------------------------------------------------

struct TestDaemon {
    socket: PathBuf,
    handle: JoinHandle<()>,
    _home: tempfile::TempDir,
}

fn unique_socket() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/airc-wcsp-{}-{n}.sock", std::process::id()))
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

async fn collect_envelopes(
    socket: PathBuf,
    channel: RoomId,
    want: usize,
    ready: Arc<Barrier>,
) -> Vec<Envelope> {
    let client = DaemonClient::new(socket);
    let mut stream = client
        .attach(AttachRequest {
            channel: Some(channel),
            from: None,
            ..Default::default()
        })
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

/// Card 75b54d0a — work outlives any participant.
///
/// Two observers, one card, Alice claims → goes dark → Bob reclaims
/// after the lease decays → Bob completes. Both observers' final
/// projections agree the card is `Closed` with no live claim and Bob
/// as the (last) owner — never Alice. The substrate never asked Alice
/// to come back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_holder_lease_expires_and_a_different_peer_reclaims() {
    let daemon = start_daemon().await;
    let channel = RoomId::new();

    // Two observers — proves reclaim is visible to "any attached peer",
    // not just the new claimant.
    const EVENTS: usize = 6;
    let ready = Arc::new(Barrier::new(3));
    let obs_a = tokio::spawn(collect_envelopes(
        daemon.socket.clone(),
        channel,
        EVENTS,
        ready.clone(),
    ));
    let obs_b = tokio::spawn(collect_envelopes(
        daemon.socket.clone(),
        channel,
        EVENTS,
        ready.clone(),
    ));
    ready.wait().await;

    // Identities.
    let alice = PeerId::from_u128(0xA11CE);
    let bob = PeerId::from_u128(0xB0B);
    let card_id = WorkCardId::from_u128(0x75B5_4D0A);
    let alice_claim = ClaimId::from_u128(0x000A_11CE_C1A1);
    let bob_claim = ClaimId::from_u128(0x00B0_BC1A_1000);

    // The churn transcript. Times are synthetic; the projection's
    // arbitration is purely event-driven — `expires_at_ms` is
    // `claimed_at_ms + ttl_ms`, and a later `CardClaimed` whose
    // `claimed_at_ms` is past the prior `expires_at_ms` reclaims.
    // The substrate has no wall-clock dependency for the proof.
    let alice_ttl_ms: u64 = 100;
    let bob_ttl_ms: u64 = 100;
    let alice_claim_at: u64 = 2;
    let alice_expires_at: u64 = alice_claim_at + alice_ttl_ms; // 102
    let bob_claim_at: u64 = 200; // > alice_expires_at — lease has decayed
    let bob_closed_at: u64 = 300;
    let bob_released_at: u64 = 301;

    let transcript: Vec<WorkEvent> = vec![
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: repo(),
            title: "75b54d0a — work outlives Alice".into(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: alice,
            created_at_ms: 1,
            reviews: None,
        }),
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: alice_claim,
            owner: alice,
            ttl_ms: alice_ttl_ms,
            claimed_at_ms: alice_claim_at,
        }),
        // ... Alice goes dark here. No heartbeat, no release.
        // Time advances past `alice_expires_at`. Bob reclaims.
        WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: bob_claim,
            owner: bob,
            ttl_ms: bob_ttl_ms,
            claimed_at_ms: bob_claim_at,
        }),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id,
            state: CardState::InProgress,
            changed_by: bob,
            changed_at_ms: bob_claim_at + 1,
        }),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id,
            state: CardState::Closed,
            changed_by: bob,
            changed_at_ms: bob_closed_at,
        }),
        WorkEvent::ClaimReleased(ClaimReleased {
            card_id,
            claim_id: bob_claim,
            owner: bob,
            reason: None,
            released_at_ms: bob_released_at,
        }),
    ];
    assert_eq!(transcript.len(), EVENTS);

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

    // (1) Both observers see every transition in order.
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
        "observer A saw a different churn transcript than the publisher authored"
    );
    assert_eq!(
        decoded_b, transcript,
        "observer B saw a different churn transcript than the publisher authored"
    );

    // (2) Lease expiry observable cross-peer: project only the first
    //     two events (create + Alice's claim) and ask both observers'
    //     boards what's stale at `bob_claim_at`. Alice's claim must
    //     appear — both observers concur the lease has decayed and
    //     the card is reclaim-eligible, without Alice or any
    //     coordinator participating.
    let pre_reclaim: Vec<WorkEvent> = transcript[..2].to_vec();
    let project = |events: Vec<WorkEvent>| -> WorkBoardProjection {
        WorkBoardProjection::replay_window(events).expect("replay projects")
    };
    for who in ["observer A", "observer B"] {
        let proj = project(pre_reclaim.clone());
        let stale = proj.stale_claims(bob_claim_at);
        assert_eq!(
            stale.len(),
            1,
            "{who}: lease must surface as stale before Bob reclaims (now_ms={bob_claim_at})"
        );
        let s = &stale[0];
        assert_eq!(s.card_id, card_id);
        assert_eq!(s.claim_id, alice_claim);
        assert_eq!(s.owner, alice);
        assert_eq!(s.expired_at_ms, alice_expires_at);

        // ... and BEFORE the lease decayed (right at the wire),
        // nothing is stale. The bound matters — it's the line the
        // arbitration draws between "Alice still owns this" and
        // "anyone can take it".
        assert!(
            proj.stale_claims(alice_expires_at - 1).is_empty(),
            "{who}: claim must not surface stale while lease is live"
        );
    }

    // (3,4) Replaying the FULL transcript on both sides agrees on
    //       the post-churn world. Bob owns the last live claim
    //       (until release), the card is Closed, and after the
    //       release the claim is cleared — no orphan claim stuck
    //       on Alice. The substrate never required Alice to come
    //       back.
    let proj_a = project(decoded_a);
    let proj_b = project(decoded_b);
    for (who, proj) in [("observer A", &proj_a), ("observer B", &proj_b)] {
        let card = proj
            .card(card_id)
            .unwrap_or_else(|| panic!("{who}: card present"));
        assert_eq!(card.state, CardState::Closed, "{who}: card terminal");
        assert_eq!(
            card.claim_id, None,
            "{who}: no orphan claim after Bob's release"
        );
        assert_eq!(card.owner, None, "{who}: no orphan owner after release");
        assert_eq!(
            card.created_by, alice,
            "{who}: creation attribution preserved (Alice authored the card)"
        );
        assert!(
            proj.stale_claims(u64::MAX).is_empty(),
            "{who}: no claim left in any expired state"
        );
    }

    daemon.stop().await;
}
