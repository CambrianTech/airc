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
    let channel_name = airc_lib::subscriptions::ChannelName::new("general").expect("channel");
    let remote_peer = PeerId::new();
    let remote_keypair = airc_protocol::PeerKeypair::generate();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    let document = airc_lib::AccountRegistryDocument::new(
        identity.clone(),
        now_ms,
        vec![channel_name.clone()],
        vec![airc_lib::AccountPeerBeacon {
            presence: airc_lib::beacon_now(
                remote_peer,
                "/machine/remote/.airc".into(),
                vec![channel_name.clone()],
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

    // The heal: the frame's channel derives from a registry-known name
    // → rebound from the registry beacon and DELIVERED.
    let general = airc_lib::subscriptions::derive_room_id(&identity, &channel_name);
    assert_eq!(
        bridge
            .deliver(&inbound_frame(general, EventId::new(), "hello blind room"))
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
            .deliver(&inbound_frame(general, EventId::new(), "second frame"))
            .await,
        InboundDeliveryVerdict::Delivered,
    );
    assert_eq!(
        rebinds(&sink),
        1,
        "a persisted binding needs no second rebind"
    );
}

/// what this catches (self-healing join, the M5↔bigmama blind-room
/// ROOT CAUSE — live log: 24 unknown_channel frames on channel
/// `eef18336-…` = derive_room_id("local:unknown-host:unknown-user",
/// "general")): bigmama's Windows daemon fell to the degenerate
/// mesh-identity fallback (gh unreachable; POSIX env probes don't
/// exist on Windows), so its `#general` UUID diverged from this
/// machine's — and every room frame between the two machines died as
/// unknown_channel while the room was bound and readable on both
/// sides the whole time. The bridge must re-derive the room from the
/// frame's channel-NAME header under THIS machine's identity and
/// deliver into the bound room (loudly); a frame with no header, or a
/// name nobody binds, keeps the honest unknown-channel verdict; and a
/// BOUND channel is never re-routed by the hint. Mutation checks:
/// dropping `reconverge_by_name` fails the DeliveredRemapped assert;
/// remapping before the bound-check would fail the never-re-route
/// assert; publishing under the addressed UUID instead of the local
/// one would fail the operator-visibility assert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diverged_channel_uuid_reconverges_by_name_header() {
    let machine = Machine::boot().await;
    let store = machine_coordinator_store(&machine).await;
    pin_identity(store.as_ref(), "converged-account").await;
    let operator = machine.attach("operator").await;
    let room = operator.join("general").await.expect("operator joins");

    let sink = MemoryDiagnosticSink::default();
    let bridge = RouterInboundBridge::new(machine.daemon.router(), store.clone())
        .with_diagnostic_sink(Arc::new(sink.clone()));

    // The literal field fingerprint: the sender derived #general under
    // the degenerate Windows fallback identity.
    let diverged_identity =
        airc_lib::subscriptions::MeshIdentity::new("local:unknown-host:unknown-user");
    let name = airc_lib::subscriptions::ChannelName::new("general").expect("channel name");
    let diverged = airc_lib::subscriptions::derive_room_id(&diverged_identity, &name);
    assert_ne!(
        diverged, room.channel,
        "premise: the two identities must derive different room UUIDs"
    );

    // Control: no name header → honestly blind (durable + loud, as before).
    assert_eq!(
        bridge
            .deliver(&inbound_frame(diverged, EventId::new(), "no hint"))
            .await,
        InboundDeliveryVerdict::UnknownChannel,
        "without the name header there is no honest reconvergence"
    );

    // THE heal: the name header re-derives to the bound local room.
    let healed_id = EventId::new();
    assert_eq!(
        bridge
            .deliver(&inbound_frame_with_headers(
                diverged,
                healed_id,
                "hello from the diverged machine",
                &[(airc_protocol::HEADER_AIRC_CHANNEL_NAME, "general")],
            ))
            .await,
        InboundDeliveryVerdict::DeliveredRemapped(room.channel),
        "a name that derives to a bound local room must deliver there"
    );
    let recent = operator.page_recent(32).await.expect("page_recent");
    assert!(
        recent.iter().any(|event| event.event_id == healed_id),
        "the reconverged frame must surface in the room the operator reads"
    );
    let reconvergences = |sink: &MemoryDiagnosticSink| {
        sink.events()
            .iter()
            .filter(|event| event.code == DiagnosticCode::ChannelNameReconverged)
            .count()
    };
    assert_eq!(reconvergences(&sink), 1, "the heal must be LOUD");

    // A name nobody binds (and the account does not know) must not
    // fake a delivery.
    assert_eq!(
        bridge
            .deliver(&inbound_frame_with_headers(
                diverged,
                EventId::new(),
                "nobody reads this",
                &[(airc_protocol::HEADER_AIRC_CHANNEL_NAME, "nobody-binds-this")],
            ))
            .await,
        InboundDeliveryVerdict::UnknownChannel,
        "an unbound name must keep the honest unknown-channel verdict"
    );

    // A BOUND channel is never re-routed: the header is a heal hint,
    // not addressing authority.
    assert_eq!(
        bridge
            .deliver(&inbound_frame_with_headers(
                room.channel,
                EventId::new(),
                "correctly addressed",
                &[(airc_protocol::HEADER_AIRC_CHANNEL_NAME, "nobody-binds-this")],
            ))
            .await,
        InboundDeliveryVerdict::Delivered,
        "a bound channel must deliver as addressed, hint ignored"
    );
    assert_eq!(
        reconvergences(&sink),
        1,
        "no reconvergence may fire for a bound or unhealable channel"
    );
}

/// what this catches (the field scenario END-TO-END, over a real TLS
/// LAN link): a remote machine pinned to the degenerate Windows
/// fallback identity joins #general (deriving a diverged channel
/// UUID), dials in, and sends with a delivery ack. Sender-side, the
/// room name must ride `HEADER_AIRC_CHANNEL_NAME` (stamped by
/// `send_frame_to_room`); receiver-side, the bridge must reconverge by
/// name; the ack must say DELIVERED carrying the receiver's LOCAL
/// channel; and the operator scope must actually see the message.
/// This is the exact exchange that produced bigmama's unknown_channel
/// receipts before the heal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diverged_remote_lan_send_reconverges_and_acks_delivered() {
    let machine = Machine::boot().await;
    let store = machine_coordinator_store(&machine).await;
    pin_identity(store.as_ref(), "converged-account").await;
    let operator = machine.attach("operator").await;
    let room = operator.join("general").await.expect("operator joins");

    let (gateway, _bridge) = lan_gateway_with_bridge(&machine).await;
    let remote_home = TempDir::new().expect("remote home");
    let remote = Airc::open(remote_home.path().join(".airc"))
        .await
        .expect("open remote");
    // Pin BEFORE join so the diverged identity shapes the remote's
    // room derivation — the bigmama boot order.
    pin_identity(
        remote.coordinator_store_for_test(),
        "local:unknown-host:unknown-user",
    )
    .await;
    let remote_room = remote.join("general").await.expect("remote joins");
    assert_ne!(
        remote_room.channel, room.channel,
        "premise: the remote must derive a DIVERGED channel UUID"
    );
    let remote_spec: PeerSpec = remote.peer_spec().parse().expect("remote spec");
    let gateway_spec: PeerSpec = gateway.peer_spec().parse().expect("gateway spec");
    gateway.add_peer(remote_spec).await.expect("gateway trusts");
    remote.add_peer(gateway_spec).await.expect("remote trusts");
    let addr: SocketAddr = gateway
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("gateway listens");
    remote
        .connect_lan(addr, gateway.peer_id())
        .await
        .expect("remote dials");

    let outcome = remote
        .send_with_delivery_ack(
            "marker across the diverged channel",
            Headers::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("send succeeds");
    let event_id = match outcome {
        DeliverySendOutcome::Delivered { event_id, ack } => {
            match ack.outcome {
                DeliveryOutcome::Delivered { channel, .. } => assert_eq!(
                    channel, room.channel,
                    "the ack must carry the receiver's LOCAL room, not the diverged UUID"
                ),
                DeliveryOutcome::Undeliverable { .. } => {
                    panic!("delivered outcome must not be undeliverable")
                }
            }
            event_id
        }
        DeliverySendOutcome::Undeliverable { .. } | DeliverySendOutcome::NoAck { .. } => {
            panic!("expected Delivered after name reconvergence, got {outcome:?}")
        }
    };

    let recent = operator.page_recent(32).await.expect("page_recent");
    let seen = recent
        .iter()
        .find(|event| event.event_id == event_id)
        .expect("the diverged remote's message must surface in the operator's room");
    assert_eq!(
        seen.headers
            .get(airc_protocol::HEADER_AIRC_CHANNEL_NAME)
            .map(String::as_str),
        Some("general"),
        "the sender must stamp the channel name on the wire — the convergence key"
    );
}

/// what this catches (self-healing join item 2 — receive-binding
/// re-derive on identity heal; the live sequel to the d79843c heal):
/// a scope that joined WHILE its machine's mesh identity was diverged
/// stores its subscription under the diverged room UUID. After the
/// identity heals, inbound frames arrive addressed to the CONVERGED
/// UUID — the per-frame name reconvergence heals delivery TO a bound
/// room, but a room bound under a stale UUID must be REBOUND or the
/// scope keeps reading the dead room (live evidence: it took a manual
/// `airc stop && airc join`). The join-shaped touchpoint must re-derive
/// and re-bind; after that, a frame addressed to the converged UUID is
/// delivered AND visible to the scope, while the old diverged UUID
/// keeps working via the existing name reconvergence remap. Mutation
/// checks: dropping `rebind_diverged` (or not calling it from `join`)
/// fails the room-UUID assert and the operator-visibility assert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healed_identity_rebinds_stale_subscription_to_the_converged_room() {
    let machine = Machine::boot().await;
    let store = machine_coordinator_store(&machine).await;
    let name = airc_lib::subscriptions::ChannelName::new("general").expect("channel name");

    // Boot diverged (the Windows-fallback shape), join #general.
    pin_identity(store.as_ref(), "diverged-boot-identity").await;
    let operator = machine.attach("operator").await;
    let stale_room = operator.join("general").await.expect("diverged join");

    let diverged_id = airc_lib::subscriptions::derive_room_id(
        &airc_lib::subscriptions::MeshIdentity::new("diverged-boot-identity"),
        &name,
    );
    let converged_id = airc_lib::subscriptions::derive_room_id(
        &airc_lib::subscriptions::MeshIdentity::new("converged-account"),
        &name,
    );
    assert_eq!(stale_room.channel, diverged_id, "premise: joined diverged");
    assert_ne!(diverged_id, converged_id, "premise: identities diverge");

    // The identity HEALS (gh reachable again / operator re-pin).
    pin_identity(store.as_ref(), "converged-account").await;

    // The join-shaped touchpoint re-binds the stored subscription.
    let healed_room = operator.join("general").await.expect("healed join");
    assert_eq!(
        healed_room.channel, converged_id,
        "join after the identity heal must re-bind the stored subscription \
         to the converged room UUID, not keep returning the stale one"
    );

    // An inbound frame addressed to the CONVERGED UUID now delivers…
    let sink = MemoryDiagnosticSink::default();
    let bridge = RouterInboundBridge::new(machine.daemon.router(), store.clone())
        .with_diagnostic_sink(Arc::new(sink.clone()));
    let converged_event = EventId::new();
    assert_eq!(
        bridge
            .deliver(&inbound_frame(
                converged_id,
                converged_event,
                "addressed to the converged room"
            ))
            .await,
        InboundDeliveryVerdict::Delivered,
        "the healed binding must make the converged room deliverable"
    );
    // …and the SCOPE actually reads it (the read side is what the
    // per-frame remap could never heal).
    let recent = operator.page_recent(32).await.expect("page_recent");
    assert!(
        recent.iter().any(|event| event.event_id == converged_event),
        "the rebound scope must see frames addressed to the converged room"
    );

    // The OLD diverged UUID keeps working via the existing name
    // reconvergence remap (a not-yet-healed sender still converges).
    let stale_addressed = EventId::new();
    assert_eq!(
        bridge
            .deliver(&inbound_frame_with_headers(
                diverged_id,
                stale_addressed,
                "addressed to the old diverged room",
                &[(airc_protocol::HEADER_AIRC_CHANNEL_NAME, "general")],
            ))
            .await,
        InboundDeliveryVerdict::DeliveredRemapped(converged_id),
        "the old UUID must keep delivering via the per-frame name remap"
    );
    let recent = operator.page_recent(32).await.expect("page_recent");
    assert!(
        recent.iter().any(|event| event.event_id == stale_addressed),
        "frames remapped from the old UUID must surface in the rebound room"
    );
}
