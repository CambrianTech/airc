//! Card 1998f6cb — routed-ack: ordinary room messages must traverse
//! established LAN routes in BOTH directions, with truthful delivery
//! semantics.
//!
//! After #1156, inbound LAN frames reach the daemon's router (and
//! every subscribed scope). This file proves the OUTBOUND mirror: a
//! local `airc send` (IPC → `router.publish`) is offered to the route
//! layer and forwarded over the EXISTING lan-tcp connection to peer
//! daemons, whose inbound bridge publishes it for their scopes —
//! end-to-end over real TLS between in-process daemons (hermetic temp
//! homes, RAII teardown, no production daemon or route touched).
//!
//! Pinned here:
//!   1. THE acceptance test: a room send on machine A appears exactly
//!      once in machine B's subscribed scope transcript — and the
//!      reverse direction too.
//!   2. Loop prevention: B never forwards A's frame back to A (origin
//!      link check — observed on B's forwarded-frame counter, which
//!      counts wire flushes, not deliveries).
//!   3. Transitive mesh forwarding terminates: A—B—C line topology
//!      delivers A's message to C exactly once, with no echo storm.
//!   4. The ring-marked-but-unpersisted retry edge: the remote
//!      persists but REPORTS failure (ack-uncertainty window) → the
//!      forwarder retries with the SAME event identity → the remote
//!      dedups (`publish_if_new`) and acks `delivered` — exactly one
//!      visible copy, no false `Duplicate`-into-`Delivered` for a
//!      frame that never landed (#1156's poisoning fix is the other
//!      half of this contract).
//!   5. Backpressure is loud: a saturated per-peer forward queue emits
//!      `RoutedForwardQueueSaturated`; an unconfirmed forward (hollow
//!      peer that never acks) emits `RoutedForwardFailed`. Never
//!      silent.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use airc_core::{Body, EventId};
use airc_diagnostics::{DiagnosticCode, MemoryDiagnosticSink};
use airc_lib::{
    Airc, InboundDeliveryVerdict, InboundFrameSink, PeerSpec, RoutedForwarder,
    RoutedForwarderConfig, RouterInboundBridge,
};
use airc_protocol::{Frame, PeerKeyRegistry, PeerKeypair};
use airc_store::{EventStore, SqliteEventStore};
use airc_transport::LanTcpAdapter;
use async_trait::async_trait;
use common::Machine;

const ROOM: &str = "routed-forward-room";

/// One simulated machine with the FULL production daemon wiring of
/// `run_daemon`: an inbound `RouterInboundBridge` and an outbound
/// `RoutedForwarder` installed on the daemon's router, sharing one
/// LAN-gateway handle (the in-process stand-in for the daemon's
/// listener/dialer handles).
struct LinkedMachine {
    machine: Machine,
    gateway: Airc,
    forwarder: RoutedForwarder,
}

async fn coordinator_store(machine: &Machine) -> Arc<dyn EventStore> {
    Arc::new(
        SqliteEventStore::open_path(&machine.wire_root().join("events.sqlite"))
            .await
            .expect("open machine coordinator store"),
    )
}

async fn boot_linked(config: RoutedForwarderConfig) -> LinkedMachine {
    let machine = Machine::boot().await;
    let gateway = boot_gateway(&machine, None).await;
    let forwarder = RoutedForwarder::install(&machine.daemon.router(), config);
    forwarder.add_link(gateway.clone()).await;
    LinkedMachine {
        machine,
        gateway,
        forwarder,
    }
}

/// Open a LAN-gateway handle on `machine` with the (optionally
/// wrapped) inbound bridge installed — exactly what `run_daemon` does
/// for its transport-owning handles.
async fn boot_gateway(
    machine: &Machine,
    wrap: Option<&dyn Fn(Arc<RouterInboundBridge>) -> Arc<dyn InboundFrameSink>>,
) -> Airc {
    let bridge = Arc::new(RouterInboundBridge::new(
        machine.daemon.router(),
        coordinator_store(machine).await,
    ));
    let gateway = Airc::open_with_wire_root_for_test(
        machine.wire_root().join("lan-gateway"),
        machine.wire_root().to_path_buf(),
    )
    .await
    .expect("open lan gateway handle");
    match wrap {
        Some(wrap) => gateway.set_inbound_frame_sink(wrap(bridge)),
        None => gateway.set_inbound_frame_sink(bridge),
    }
    gateway
}

/// Establish the LAN route `dialer → listener` (mutual trust + TLS),
/// mirroring what stored-endpoint route discovery produces. Returns
/// the listener's bound address.
async fn link(dialer: &Airc, listener: &Airc) -> SocketAddr {
    common::trust(dialer, listener).await;
    let addr = listener
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("listener binds");
    dialer
        .connect_lan(addr, listener.peer_id())
        .await
        .expect("dialer connects");
    addr
}

/// Poll a scope's transcript until `event_id` appears (10s budget),
/// then return how many copies are visible.
async fn wait_for_copies(scope: &Airc, event_id: EventId) -> usize {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let recent = scope.page_recent(64).await.expect("page_recent");
        let copies = recent.iter().filter(|e| e.event_id == event_id).count();
        if copies > 0 {
            return copies;
        }
        if tokio::time::Instant::now() >= deadline {
            return 0;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn copies_in(events: &[airc_core::TranscriptEvent], event_id: EventId) -> usize {
    events.iter().filter(|e| e.event_id == event_id).count()
}

/// THE acceptance test (the card): an ordinary room send on daemon A —
/// the routed IPC path, NOT the point-to-point `lan-send` verb —
/// appears in daemon B's subscribed scope transcript, and the reverse
/// direction works over the same single TLS connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routed_room_send_traverses_lan_both_directions() {
    let a = boot_linked(RoutedForwarderConfig::default()).await;
    let b = boot_linked(RoutedForwarderConfig::default()).await;
    link(&a.gateway, &b.gateway).await;

    let op_a = a.machine.attach("op-a").await;
    op_a.join(ROOM).await.expect("op-a joins");
    let op_b = b.machine.attach("op-b").await;
    op_b.join(ROOM).await.expect("op-b joins");

    // A → B: the leg that was dead (daemon-originated ROUTED sends).
    let id_ab = op_a
        .say("ordinary room message, machine A to machine B")
        .await
        .expect("op-a says");
    assert_eq!(
        wait_for_copies(&op_b, id_ab).await,
        1,
        "machine A's room send must appear exactly once in machine B's \
         subscribed scope transcript"
    );

    // B → A over the SAME established connection (no re-dial: B never
    // dialed A; its only route is the connection A opened).
    let id_ba = op_b
        .say("and back again, machine B to machine A")
        .await
        .expect("op-b says");
    assert_eq!(
        wait_for_copies(&op_a, id_ba).await,
        1,
        "machine B's room send must traverse the same LAN connection back \
         to machine A's subscribed scope"
    );

    // Truthful semantics: each forwarder got a delivered confirmation.
    assert!(
        a.forwarder.confirmed_count() >= 1,
        "A→B must be ack-confirmed"
    );
    assert!(
        b.forwarder.confirmed_count() >= 1,
        "B→A must be ack-confirmed"
    );

    // Exactly-once at the source too (no echo doubled anything).
    let recent_a = op_a.page_recent(64).await.expect("page op-a");
    assert_eq!(copies_in(&recent_a, id_ab), 1);
    let recent_b = op_b.page_recent(64).await.expect("page op-b");
    assert_eq!(copies_in(&recent_b, id_ba), 1);
}

/// Loop prevention: a frame B received FROM A must never be forwarded
/// back to A. B's `forwarded_count` counts frames flushed to the wire
/// — with A as B's only link and B originating nothing after the
/// snapshot, it must not move. (Removing the origin check makes B echo
/// the frame to A and this counter increments — the mutation this test
/// exists to catch; A would dedup the echo, so transcript counts alone
/// cannot see it.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frame_received_from_a_peer_is_not_forwarded_back_to_it() {
    let a = boot_linked(RoutedForwarderConfig::default()).await;
    let b = boot_linked(RoutedForwarderConfig::default()).await;
    link(&a.gateway, &b.gateway).await;

    let op_a = a.machine.attach("op-a").await;
    op_a.join(ROOM).await.expect("op-a joins");
    let op_b = b.machine.attach("op-b").await;
    op_b.join(ROOM).await.expect("op-b joins");

    // Quiesce, then snapshot B's wire-flush counter.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let b_forwarded_before = b.forwarder.forwarded_count();

    let event_id = op_a.say("must not boomerang").await.expect("op-a says");
    assert_eq!(wait_for_copies(&op_b, event_id).await, 1);

    // Give a hypothetical echo ample time to be flushed, then assert
    // B forwarded nothing: its only connected peer is the frame's
    // origin link.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        b.forwarder.forwarded_count(),
        b_forwarded_before,
        "B's only link is the origin of the frame — forwarding anything \
         means the origin check is gone (echo back to A)"
    );

    // Belt: A's transcript still holds exactly one copy.
    let recent_a = op_a.page_recent(64).await.expect("page op-a");
    assert_eq!(copies_in(&recent_a, event_id), 1);
}

/// Transitive mesh forwarding across a line topology A—B—C: A's
/// message reaches C (B re-forwards on first acceptance), exactly
/// once, and the flood terminates (duplicates are dead-ends, origin
/// links are excluded) — no echo storm back through A.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transitive_forward_reaches_third_machine_exactly_once() {
    let a = boot_linked(RoutedForwarderConfig::default()).await;
    let b = boot_linked(RoutedForwarderConfig::default()).await;
    let c = boot_linked(RoutedForwarderConfig::default()).await;
    // A dials B; B dials C. A and C share no connection.
    link(&a.gateway, &b.gateway).await;
    link(&b.gateway, &c.gateway).await;

    let op_a = a.machine.attach("op-a").await;
    op_a.join(ROOM).await.expect("op-a joins");
    let op_b = b.machine.attach("op-b").await;
    op_b.join(ROOM).await.expect("op-b joins");
    let op_c = c.machine.attach("op-c").await;
    op_c.join(ROOM).await.expect("op-c joins");

    let event_id = op_a
        .say("one hop, two hops — exactly once everywhere")
        .await
        .expect("op-a says");

    assert_eq!(
        wait_for_copies(&op_b, event_id).await,
        1,
        "one hop: B's scope sees the message exactly once"
    );
    assert_eq!(
        wait_for_copies(&op_c, event_id).await,
        1,
        "two hops: C's scope sees the message exactly once (B re-forwarded \
         its first acceptance toward C, origin A excluded)"
    );

    // Termination: let any echo settle, then re-assert exactly-once
    // everywhere (C's only non-origin peer set is empty; a hypothetical
    // echo through the mesh dead-ends as Duplicate at every node).
    tokio::time::sleep(Duration::from_millis(500)).await;
    for (name, op) in [("A", &op_a), ("B", &op_b), ("C", &op_c)] {
        let recent = op.page_recent(64).await.expect("page_recent");
        assert_eq!(
            copies_in(&recent, event_id),
            1,
            "machine {name} must hold exactly one transcript copy after the flood settles"
        );
    }
}

/// The ring-marked-but-unpersisted retry edge (card body): the remote
/// ACCEPTS the frame into its router but its verdict reports failure
/// (the ack-uncertainty window — persisted-but-unconfirmed). The
/// forwarder must retry with the SAME event identity so the remote's
/// `publish_if_new` dedups the retry into a truthful `delivered`
/// (duplicate IS delivered) — exactly one visible copy. A forwarder
/// that mints fresh identity per retry double-delivers here.
struct PersistedButReportedFailed {
    inner: Arc<RouterInboundBridge>,
    marker: &'static str,
    injected: AtomicBool,
}

#[async_trait]
impl InboundFrameSink for PersistedButReportedFailed {
    async fn deliver(&self, frame: &Frame) -> InboundDeliveryVerdict {
        let verdict = self.inner.deliver(frame).await;
        let is_marked = frame
            .envelope
            .body
            .as_ref()
            .and_then(Body::as_text)
            .is_some_and(|text| text == self.marker);
        if is_marked && !self.injected.swap(true, Ordering::SeqCst) {
            // The frame IS in the router (verdict above ran) — but the
            // sender is told it failed, exactly the crash-window shape
            // where the persist landed and the confirmation did not.
            return InboundDeliveryVerdict::Failed(
                "injected: persisted but unconfirmed (ack-uncertainty window)".to_string(),
            );
        }
        verdict
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_persist_failure_then_retry_is_exactly_once_visible() {
    const MARKER: &str = "retry me without changing my identity";

    let a = boot_linked(RoutedForwarderConfig {
        ack_timeout: Duration::from_secs(5),
        max_attempts: 3,
        retry_backoff: Duration::from_millis(100),
        ..RoutedForwarderConfig::default()
    })
    .await;

    // Machine B with the flaky verdict wrapped around the REAL bridge.
    let b_machine = Machine::boot().await;
    let wrap = |bridge: Arc<RouterInboundBridge>| -> Arc<dyn InboundFrameSink> {
        Arc::new(PersistedButReportedFailed {
            inner: bridge,
            marker: MARKER,
            injected: AtomicBool::new(false),
        })
    };
    let b_gateway = boot_gateway(&b_machine, Some(&wrap)).await;
    let b_forwarder = RoutedForwarder::install(&b_machine.daemon.router(), Default::default());
    b_forwarder.add_link(b_gateway.clone()).await;

    link(&a.gateway, &b_gateway).await;

    let op_a = a.machine.attach("op-a").await;
    op_a.join(ROOM).await.expect("op-a joins");
    let op_b = b_machine.attach("op-b").await;
    op_b.join(ROOM).await.expect("op-b joins");

    let event_id = op_a.say(MARKER).await.expect("op-a says");

    // The retry (same event_id) must conclude delivered…
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while a.forwarder.confirmed_count() == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        a.forwarder.confirmed_count(),
        1,
        "the retry of the same event identity must be ack-confirmed delivered \
         (the remote deduped it — duplicate IS delivered)"
    );

    // …and B's scope must see exactly ONE copy, despite two wire
    // arrivals of the event.
    assert_eq!(wait_for_copies(&op_b, event_id).await, 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let recent_b = op_b.page_recent(64).await.expect("page op-b");
    assert_eq!(
        copies_in(&recent_b, event_id),
        1,
        "retries must dedup by stable event identity — exactly once visible"
    );
}

/// Backpressure + failure loudness: a connected peer that NEVER acks
/// (hollow listener — accepts TLS, persists nothing, answers nothing).
/// With a 1-deep per-peer queue and a worker pinned on the ack wait,
/// a burst must overflow the queue (typed `RoutedForwardQueueSaturated`)
/// and every unconfirmed forward must end in a typed
/// `RoutedForwardFailed`. Nothing is silent, and local delivery is
/// untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_forward_queue_and_unconfirmed_forwards_are_loud() {
    let a = boot_linked(RoutedForwarderConfig {
        peer_queue_capacity: 1,
        ack_timeout: Duration::from_millis(800),
        max_attempts: 1,
        retry_backoff: Duration::from_millis(50),
        ..RoutedForwarderConfig::default()
    })
    .await;
    let diag = MemoryDiagnosticSink::default();
    a.forwarder.set_diagnostic_sink(Arc::new(diag.clone()));

    // Hollow peer: TLS-pinned listener with NO ingest behind it.
    let hollow_id = airc_core::PeerId::from_u128(0x710c_1998_f6cb);
    let hollow_kp = PeerKeypair::generate();
    let hollow_pubkey = hollow_kp.public_bytes();
    let gateway_spec: PeerSpec = a.gateway.peer_spec().parse().expect("gateway spec");
    let registry = PeerKeyRegistry::new();
    registry
        .enrol(hollow_id, 0, hollow_pubkey)
        .expect("enrol hollow self");
    registry
        .enrol(gateway_spec.peer_id, 0, gateway_spec.pubkey)
        .expect("enrol gateway");
    let hollow =
        LanTcpAdapter::new(hollow_id, hollow_kp, Arc::new(registry)).expect("hollow adapter");
    a.gateway
        .add_peer(PeerSpec {
            peer_id: hollow_id,
            pubkey: hollow_pubkey,
        })
        .await
        .expect("gateway trusts hollow");
    let hollow_addr = hollow
        .listen(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("hollow listens");
    a.gateway
        .connect_lan(hollow_addr, hollow_id)
        .await
        .expect("gateway dials hollow");

    let op_a = a.machine.attach("op-a").await;
    op_a.join(ROOM).await.expect("op-a joins");

    // Burst: worker pins on item 1's ack wait, item 2 queues, the rest
    // overflow the 1-deep peer queue.
    for n in 0..5 {
        op_a.say(&format!("burst {n}"))
            .await
            .expect("local send must keep working while the route is dead");
    }

    // Saturation diagnostic is synchronous with the burst.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let events = diag.events();
        let saturated = events
            .iter()
            .any(|e| e.code == DiagnosticCode::RoutedForwardQueueSaturated);
        let failed = events
            .iter()
            .any(|e| e.code == DiagnosticCode::RoutedForwardFailed);
        if saturated && failed {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected RoutedForwardQueueSaturated + RoutedForwardFailed diagnostics; got {:?}",
            events
                .iter()
                .map(|e| (e.code, e.message.clone()))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Local transcript is intact: all 5 messages visible locally.
    let recent = op_a.page_recent(16).await.expect("page op-a");
    let burst_count = recent
        .iter()
        .filter(|e| {
            e.body
                .as_ref()
                .and_then(Body::as_text)
                .is_some_and(|t| t.starts_with("burst "))
        })
        .count();
    assert_eq!(
        burst_count, 5,
        "loud-drop is about the WIRE leg only — local delivery never degrades"
    );

    // And nothing was ever confirmed: the ack vocabulary stayed truthful.
    assert_eq!(a.forwarder.confirmed_count(), 0);
}
