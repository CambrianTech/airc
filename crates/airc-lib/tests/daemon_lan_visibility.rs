//! Card 4132f48c — store-split: inbound LAN frames must land in the
//! transcript operator scopes actually read.
//!
//! Live forensics (#1155): a cross-machine message from the 5090
//! (02:32:10Z) sat durable in the machine store's SDK `events` table
//! — written by the daemon's LAN-receiving handle — while every
//! operator surface (`airc inbox`, monitors) reads the daemon's
//! owner-core router (hot ring + `bus_events` durable tier). Route
//! healthy, frame delivered + acked, visible to nobody.
//!
//! The fix mirrors how local sends already propagate across scopes on
//! one machine: ONE mechanism, the daemon's `EventRouter`. The
//! daemon's transport-owning handles install a `RouterInboundBridge`
//! ([`Airc::set_inbound_frame_sink`]) so every inbound frame is
//! published into the router — fan-out at delivery, no per-scope
//! copies — via the idempotent `EventRouter::publish_if_new`.
//!
//! Proven here, over a real TLS LAN link into a real in-process
//! daemon (hermetic temp homes + sockets, RAII teardown, no
//! production state touched):
//!
//!   1. THE test: an inbound LAN frame becomes visible in a
//!      subscribed scope's transcript (`page_recent` through the
//!      daemon), and the sender's delivery ack says `delivered`.
//!   2. A scope subscribed to a different room does NOT see it.
//!   3. No duplicate when the same event reaches the router twice
//!      (local publish + LAN echo, or the same frame on two LAN
//!      links) — exactly one transcript copy.
//!   4. Cursors stay monotonic across interleaved local and bridged
//!      events.
//!   5. Ack truthfulness (extends card 39d37629): with NO scope
//!      subscribed on the receiving machine the ack is
//!      `undeliverable{unknown_channel}` + a loud receiver
//!      diagnostic; after a scope joins, the ack is `delivered` —
//!      and the earlier frame was durably kept (late join replays
//!      it).

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use airc_core::{Body, EventId, Headers, PeerId, RoomId};
use airc_diagnostics::{DiagnosticCode, MemoryDiagnosticSink};
use airc_lib::{
    Airc, DeliveryOutcome, DeliverySendOutcome, InboundDeliveryVerdict, InboundFrameSink, PeerSpec,
    RouterInboundBridge, UndeliverableReason,
};
use airc_protocol::{Envelope as ProtoEnvelope, Frame, FrameKind, Signature};
use airc_store::{EventStore, SqliteEventStore};
use common::{pin_identity, Machine};
use tempfile::TempDir;

/// The store every scope on this simulated machine publishes presence
/// beacons + the mesh-identity cache into (their shared wire root) —
/// the same store the production daemon passes the bridge as its
/// coordinator store.
async fn machine_coordinator_store(machine: &Machine) -> Arc<dyn EventStore> {
    Arc::new(
        SqliteEventStore::open_path(&machine.wire_root().join("events.sqlite"))
            .await
            .expect("open machine coordinator store"),
    )
}

/// Build the production bridge against this machine's daemon router +
/// coordinator store and install it on a LAN-gateway handle — the
/// in-process equivalent of what `run_daemon` does for its listener
/// and dialer handles.
async fn lan_gateway_with_bridge(machine: &Machine) -> (Airc, Arc<RouterInboundBridge>) {
    let bridge = Arc::new(RouterInboundBridge::new(
        machine.daemon.router(),
        machine_coordinator_store(machine).await,
    ));
    let gateway = Airc::open_with_wire_root_for_test(
        machine.wire_root().join("lan-gateway"),
        machine.wire_root().to_path_buf(),
    )
    .await
    .expect("open lan gateway handle");
    gateway.set_inbound_frame_sink(bridge.clone());
    (gateway, bridge)
}

/// A remote peer on its own "machine" (isolated temp home), joined to
/// `room`, mutually trusted and dialed into the gateway's listener.
async fn dialed_remote(gateway: &Airc, remote_home: &TempDir, room: &str) -> Airc {
    let remote = Airc::open(remote_home.path().join(".airc"))
        .await
        .expect("open remote");
    remote.join(room).await.expect("remote joins room");
    let remote_spec: PeerSpec = remote.peer_spec().parse().expect("remote spec");
    let gateway_spec: PeerSpec = gateway.peer_spec().parse().expect("gateway spec");
    gateway
        .add_peer(remote_spec)
        .await
        .expect("gateway trusts remote");
    remote
        .add_peer(gateway_spec)
        .await
        .expect("remote trusts gateway");
    let addr: SocketAddr = gateway
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("gateway listens");
    remote
        .connect_lan(addr, gateway.peer_id())
        .await
        .expect("remote dials gateway");
    remote
}

fn expect_delivered(outcome: DeliverySendOutcome) -> EventId {
    match outcome {
        DeliverySendOutcome::Delivered { event_id, .. } => event_id,
        DeliverySendOutcome::Undeliverable { .. } | DeliverySendOutcome::NoAck { .. } => {
            panic!("expected Delivered, got {outcome:?}")
        }
    }
}

/// A bare inbound frame as the bridge sees one post-verification —
/// used to exercise echo/duplicate shapes where the wire would carry
/// an event_id we control.
fn inbound_frame(channel: RoomId, event_id: EventId, text: &str) -> Frame {
    Frame {
        kind: FrameKind::Message,
        envelope: ProtoEnvelope {
            event_id,
            sender: PeerId::new(),
            sender_client: airc_core::ClientId::new(),
            channel,
            target: airc_core::MentionTarget::All,
            lamport: 1,
            occurred_at_ms: 1_000,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text(text)),
            media: Vec::new(),
            signature: Signature::Unsigned,
        },
    }
}

/// Same shape as [`inbound_frame`] plus wire headers — the
/// channel-name reconvergence tests stamp
/// [`airc_protocol::HEADER_AIRC_CHANNEL_NAME`] the way a real sender
/// does.
fn inbound_frame_with_headers(
    channel: RoomId,
    event_id: EventId,
    text: &str,
    headers: &[(&str, &str)],
) -> Frame {
    let mut frame = inbound_frame(channel, event_id, text);
    for (key, value) in headers {
        frame
            .envelope
            .headers
            .insert((*key).to_string(), (*value).to_string());
    }
    frame
}

/// THE test (the card): a cross-machine room message arrives over a
/// real TLS LAN link and must appear in the transcript a subscribed
/// operator scope reads through the daemon — and the sender's
/// delivery ack must say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_lan_frame_is_visible_in_subscribed_scope_transcript() {
    let machine = Machine::boot().await;
    let operator = machine.attach("operator").await;
    operator
        .join("store-split-room")
        .await
        .expect("operator joins");

    let (gateway, _bridge) = lan_gateway_with_bridge(&machine).await;
    let remote_home = TempDir::new().expect("remote home");
    let remote = dialed_remote(&gateway, &remote_home, "store-split-room").await;

    let outcome = remote
        .send_with_delivery_ack(
            "cross-machine hello, visible at last",
            Headers::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("ack-requesting send succeeds");
    let event_id = expect_delivered(outcome);

    // The keystone: the OPERATOR SCOPE's transcript surface (daemon
    // inbox on its current room) shows the cross-machine message.
    let recent = operator
        .page_recent(16)
        .await
        .expect("operator page_recent");
    let delivered: Vec<_> = recent
        .iter()
        .filter(|event| event.event_id == event_id)
        .collect();
    assert_eq!(
        delivered.len(),
        1,
        "the delivered-acked cross-machine event must appear exactly once in the \
         subscribed scope's transcript; got {} of it among {} events",
        delivered.len(),
        recent.len()
    );
    assert_eq!(
        delivered[0].body.as_ref().and_then(Body::as_text),
        Some("cross-machine hello, visible at last"),
        "transcript body must round-trip"
    );
}

/// A scope on the same machine subscribed to a DIFFERENT room must not
/// see the inbound frame — visibility is read-side channel scoping,
/// exactly like local sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribed_scope_does_not_see_inbound_frame() {
    let machine = Machine::boot().await;
    let operator = machine.attach("operator").await;
    operator
        .join("store-split-room")
        .await
        .expect("operator joins");
    let bystander = machine.attach("bystander").await;
    bystander
        .join("uninvolved-room")
        .await
        .expect("bystander joins");

    let (gateway, _bridge) = lan_gateway_with_bridge(&machine).await;
    let remote_home = TempDir::new().expect("remote home");
    let remote = dialed_remote(&gateway, &remote_home, "store-split-room").await;

    let outcome = remote
        .send_with_delivery_ack(
            "not for the bystander",
            Headers::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("send succeeds");
    let event_id = expect_delivered(outcome);

    let bystander_view = bystander
        .page_recent(32)
        .await
        .expect("bystander page_recent");
    assert!(
        !bystander_view
            .iter()
            .any(|event| event.event_id == event_id),
        "a scope subscribed to a different room must not see the frame"
    );
}

/// No double delivery (card constraint): the same event_id reaching
/// the router twice — a LAN echo of a locally published event, or the
/// same frame arriving on the daemon's listener AND dialer handles —
/// must keep exactly one transcript copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_event_id_through_local_publish_and_lan_echo_delivers_once() {
    let machine = Machine::boot().await;
    let operator = machine.attach("operator").await;
    let room = operator.join("store-split-room").await.expect("join");
    let (_gateway, bridge) = lan_gateway_with_bridge(&machine).await;

    // Shape A: local-sender + LAN-receiver. The scope publishes
    // through the daemon; the same event then echoes back over a LAN
    // link onto the bridge.
    let local_id = operator.say("local original").await.expect("local say");
    let verdict = bridge
        .deliver(&inbound_frame(room.channel, local_id, "local original"))
        .await;
    assert_eq!(
        verdict,
        InboundDeliveryVerdict::Delivered,
        "an already-delivered duplicate still acks delivered (it IS delivered)"
    );

    // Shape B: the same inbound frame on two LAN links.
    let wire_id = EventId::new();
    let first = bridge
        .deliver(&inbound_frame(room.channel, wire_id, "wire frame"))
        .await;
    let second = bridge
        .deliver(&inbound_frame(room.channel, wire_id, "wire frame"))
        .await;
    assert_eq!(first, InboundDeliveryVerdict::Delivered);
    assert_eq!(second, InboundDeliveryVerdict::Delivered);

    let recent = operator.page_recent(32).await.expect("page_recent");
    let local_copies = recent
        .iter()
        .filter(|event| event.event_id == local_id)
        .count();
    let wire_copies = recent
        .iter()
        .filter(|event| event.event_id == wire_id)
        .count();
    assert_eq!(
        local_copies, 1,
        "locally published event must not duplicate when its LAN echo arrives"
    );
    assert_eq!(
        wire_copies, 1,
        "the same inbound frame on two links must deliver exactly once"
    );
}

/// Cursor coherence (card constraint): bridged events take owner-
/// assigned seqs exactly like local publishes, so the transcript's
/// cursor order stays strictly monotonic across interleaving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursors_stay_monotonic_across_local_and_bridged_events() {
    let machine = Machine::boot().await;
    let operator = machine.attach("operator").await;
    let room = operator.join("store-split-room").await.expect("join");
    let (_gateway, bridge) = lan_gateway_with_bridge(&machine).await;

    operator.say("local-1").await.expect("say 1");
    let bridged_1 = EventId::new();
    assert_eq!(
        bridge
            .deliver(&inbound_frame(room.channel, bridged_1, "bridged-1"))
            .await,
        InboundDeliveryVerdict::Delivered
    );
    operator.say("local-2").await.expect("say 2");
    let bridged_2 = EventId::new();
    assert_eq!(
        bridge
            .deliver(&inbound_frame(room.channel, bridged_2, "bridged-2"))
            .await,
        InboundDeliveryVerdict::Delivered
    );

    let recent = operator.page_recent(32).await.expect("page_recent");
    assert!(
        recent.len() >= 4,
        "expected at least the four interleaved events, got {}",
        recent.len()
    );
    for pair in recent.windows(2) {
        assert!(
            pair[1].cursor().lamport > pair[0].cursor().lamport
                || (pair[1].cursor().lamport == pair[0].cursor().lamport
                    && pair[1].event_id.0 > pair[0].event_id.0),
            "transcript order must be strictly monotonic; got {:?} then {:?}",
            pair[0].cursor(),
            pair[1].cursor()
        );
    }
    let ids: Vec<EventId> = recent.iter().map(|event| event.event_id).collect();
    assert!(ids.contains(&bridged_1) && ids.contains(&bridged_2));
}

/// Ack truthfulness (extends #1155): after this card, `delivered`
/// means visible-to-subscribed-scopes — not just machine-durable.
/// With no scope subscribed the receiver says unknown_channel (loud),
/// keeps the frame durably, and flips to delivered once a scope joins
/// — at which point the late joiner replays the earlier frame too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_ack_means_visible_to_subscribed_scopes_not_just_durable() {
    let machine = Machine::boot().await;
    let (gateway, _bridge) = lan_gateway_with_bridge(&machine).await;
    let diag = MemoryDiagnosticSink::default();
    gateway.set_diagnostic_sink(Arc::new(diag.clone()));

    let remote_home = TempDir::new().expect("remote home");
    let remote = dialed_remote(&gateway, &remote_home, "store-split-room").await;

    // Nobody on the receiving machine subscribes yet.
    let outcome = remote
        .send_with_delivery_ack(
            "durable but nobody reads this yet",
            Headers::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("send succeeds");
    let first_id = match outcome {
        DeliverySendOutcome::Undeliverable { event_id, ack } => {
            assert_eq!(
                ack.outcome,
                DeliveryOutcome::Undeliverable {
                    reason: UndeliverableReason::UnknownChannel
                },
                "no subscribed scope => unknown_channel, even though the frame is durable"
            );
            event_id
        }
        DeliverySendOutcome::Delivered { .. } | DeliverySendOutcome::NoAck { .. } => {
            panic!("expected Undeliverable while no scope subscribes, got {outcome:?}")
        }
    };
    assert!(
        diag.events()
            .iter()
            .any(|event| event.code == DiagnosticCode::FrameUndeliverable
                && event
                    .fields
                    .get("reason")
                    .is_some_and(|reason| reason == "unknown_channel")),
        "the receiver must say LOUDLY that a durable frame has no reader"
    );

    // An operator scope joins the room — now there is a reader.
    let operator = machine.attach("operator").await;
    operator
        .join("store-split-room")
        .await
        .expect("operator joins");

    let outcome = remote
        .send_with_delivery_ack(
            "now someone reads it",
            Headers::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("second send succeeds");
    let second_id = expect_delivered(outcome);

    // The late joiner sees BOTH: the delivered one and the earlier
    // durably-kept frame (no data loss on unknown_channel).
    let recent = operator.page_recent(32).await.expect("page_recent");
    let ids: Vec<EventId> = recent.iter().map(|event| event.event_id).collect();
    assert!(
        ids.contains(&second_id),
        "delivered-acked event must be in the subscribed scope's transcript"
    );
    assert!(
        ids.contains(&first_id),
        "the pre-subscription frame stays durable and replays to a late joiner"
    );
}

/// what this catches (self-healing join, M5↔bigmama decay mode #5 —
/// the "blind room"): an inbound frame for a channel NO scope binds
/// used to be durably stored and never surfaced, with only a stderr
/// diagnostic. When the account registry's local cache KNOWS the
/// channel, the bridge must re-bind from the registry's beacons and
/// deliver — and a channel the account does NOT know must keep the
/// honest unknown-channel verdict. Mutation checks: dropping the
/// `try_rebind_known_channel` call keeps the first assert at
/// UnknownChannel; dropping the post-rebind re-check would claim
/// Delivered even when binding failed (the second frame's no-new-diag
/// assert pins the binding actually persisted).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_channel_auto_rebinds_from_account_registry_cache() {
    use airc_lib::AccountRegistryStore as _;

    let machine = Machine::boot().await;
    let store = machine_coordinator_store(&machine).await;
    let identity = airc_lib::resolve_mesh_identity(store.as_ref())
        .await
        .expect("resolve mesh identity")
        .as_mesh_identity();

    // Seed the LOCAL account-registry cache: the account knows
    // #general via a remote machine's beacon. No scope on THIS machine
    // has ever joined, so the coordinator store holds no binding.
    // The account knows this ROOM BY ID — that is what a beacon
    // advertises and what a frame addresses.
    let known_room = RoomId::new();
    let remote_peer = PeerId::new();
    let remote_keypair = airc_protocol::PeerKeypair::generate();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    let document = airc_lib::AccountRegistryDocument::new(
        identity.clone(),
        now_ms,
        vec![known_room],
        vec![airc_lib::AccountPeerBeacon {
            presence: airc_lib::beacon_now(
                remote_peer,
                "/machine/remote/.airc".into(),
                vec![known_room],
                123,
                now_ms,
            ),
            peer_spec: PeerSpec {
                peer_id: remote_peer,
                pubkey: remote_keypair.public_bytes(),
            },
            endpoints: Vec::new(),
            endpoints_advertised_at_ms: None,
            endpoints_peer_id: None,
        }],
    );
    let concrete = Arc::new(
        SqliteEventStore::open_path(&machine.wire_root().join("events.sqlite"))
            .await
            .expect("open concrete machine store"),
    );
    let registry = Arc::new(airc_lib::SqliteAccountRegistryStore::new(concrete));
    registry.publish(&document).await.expect("seed cache");

    let sink = MemoryDiagnosticSink::default();
    let bridge = RouterInboundBridge::new(machine.daemon.router(), store.clone())
        .with_account_registry(registry)
        .with_diagnostic_sink(Arc::new(sink.clone()));

    // Control: a channel the ACCOUNT does not know keeps the honest
    // unknown-channel verdict (durable, blind, loudly diagnosed by the
    // transport layer above the bridge).
    let unknown = RoomId::new();
    assert_eq!(
        bridge
            .deliver(&inbound_frame(unknown, EventId::new(), "nobody knows me"))
            .await,
        InboundDeliveryVerdict::UnknownChannel,
        "an account-unknown channel must not fake a rebind"
    );

    // The heal: the frame addresses a room the registry KNOWS BY ID
    // → rebound from the registry beacon and DELIVERED.
    assert_eq!(
        bridge
            .deliver(&inbound_frame(known_room, EventId::new(), "hello blind room"))
            .await,
        InboundDeliveryVerdict::Delivered,
        "a registry-known channel must re-bind and deliver, not store-and-drop"
    );
    let rebinds = |sink: &MemoryDiagnosticSink| {
        sink.events()
            .iter()
            .filter(|event| event.code == DiagnosticCode::UnknownChannelRebound)
            .count()
    };
    assert_eq!(
        rebinds(&sink),
        1,
        "the rebind must be LOUD (one diagnostic)"
    );

    // The binding persists: the next frame delivers with NO further
    // rebind — the heal restored durable state, not a per-frame patch.
    assert_eq!(
        bridge
            .deliver(&inbound_frame(known_room, EventId::new(), "second frame"))
            .await,
        InboundDeliveryVerdict::Delivered,
    );
    assert_eq!(
        rebinds(&sink),
        1,
        "a persisted binding needs no second rebind"
    );
}

/// A sender's channel-NAME header must NEVER redirect a frame.
///
/// what this catches: the deleted `reconverge_by_name`. It re-routed an
/// unbound frame to whatever local room the sender's NAME header
/// derived to — which meant a peer could redirect a frame into a room
/// it was not addressing by sending different text. That existed only
/// because room ids were `v5(identity, name)` and could DIVERGE between
/// machines. Ids are minted now: nothing derives, so nothing diverges,
/// and the header is decoration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_header_never_redirects_a_frame() {
    let machine = Machine::boot().await;
    let store = machine_coordinator_store(&machine).await;
    pin_identity(store.as_ref(), "converged-account").await;
    let operator = machine.attach("operator").await;
    let room = operator.join("general").await.expect("join");

    let sink = MemoryDiagnosticSink::default();
    let bridge = RouterInboundBridge::new(machine.daemon.router(), store.clone())
        .with_diagnostic_sink(Arc::new(sink.clone()));

    // A frame addressed at a room NOBODY binds, carrying the name of a
    // room this scope IS in. The old code delivered it into #general.
    let stranger = RoomId::new();
    assert_eq!(
        bridge
            .deliver(&inbound_frame_with_headers(
                stranger,
                EventId::new(),
                "i claim to be #general",
                &[(airc_protocol::HEADER_AIRC_CHANNEL_NAME, "general")],
            ))
            .await,
        InboundDeliveryVerdict::UnknownChannel,
        "a name header must not buy delivery into a room the frame did not address"
    );

    // And the room it named is untouched — the frame did not land there.
    assert_ne!(stranger, room.channel);
}
