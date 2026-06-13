//! Card 39d37629 — delivery acks for point-to-point LAN sends.
//!
//! Live repro this closes: 5090 → mac `lan-send` printed "sent over
//! lan-tcp to general" (exit 0) at 2026-06-12 02:36Z, TLS pinning
//! succeeded, and the frame appeared in NO store on the receiving
//! machine. Sender-side "sent" proved only a TLS flush; every failure
//! between transport accept and transcript persistence was silent.
//!
//! Contract proven here (two hermetic temp-home handles over a real
//! TLS LAN link; no production daemon, no production route touched):
//!
//!   1. send → delivered ack with the receiver-side channel + cursor,
//!      emitted only AFTER the receiver persisted the frame (the
//!      persistence assertion reads the receiver's store).
//!   2. send to a channel the receiving scope has NOT bound →
//!      undeliverable{unknown_channel} ack + a loud typed
//!      `frame_undeliverable` diagnostic on the receiver (asserted
//!      through an injected MemoryDiagnosticSink — suppressing the
//!      diagnostic fails this test).
//!   3. receiver alive on the wire but its ingest is gone (handle
//!      dropped mid-conversation — the "receiver killed mid-send" /
//!      old-build shape) → the sender reports NoAck, distinctly from
//!      both delivered and undeliverable.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use airc_core::Headers;
use airc_diagnostics::{DiagnosticCode, MemoryDiagnosticSink};
use airc_lib::{Airc, DeliveryOutcome, DeliverySendOutcome, UndeliverableReason};
use tempfile::TempDir;

async fn paired_handles(tmp_a: &TempDir, tmp_b: &TempDir) -> (Airc, Airc) {
    let alice = Airc::open(tmp_a.path().join(".airc"))
        .await
        .expect("alice open");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");
    let alice_spec = alice.peer_spec().parse().expect("alice spec");
    let bob_spec = bob.peer_spec().parse().expect("bob spec");
    alice.add_peer(bob_spec).await.expect("alice trusts bob");
    bob.add_peer(alice_spec).await.expect("bob trusts alice");
    (alice, bob)
}

#[tokio::test]
async fn lan_send_yields_delivered_ack_only_after_receiver_persistence() {
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let (alice, bob) = paired_handles(&tmp_a, &tmp_b).await;

    // Both scopes bind the same channel name; same-machine mesh
    // identity resolution makes the derived RoomId identical, which
    // is the cross-machine production shape for a shared room.
    let alice_room = alice.join("delivery-ack-room").await.expect("alice joins");
    bob.join("delivery-ack-room").await.expect("bob joins");

    let alice_addr: SocketAddr = alice
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("alice listens");
    bob.connect_lan(alice_addr, alice.peer_id())
        .await
        .expect("bob dials alice");

    let outcome = bob
        .send_with_delivery_ack(
            "ack me when persisted",
            Headers::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("ack-requesting send succeeds");

    let (event_id, ack) = match outcome {
        DeliverySendOutcome::Delivered { event_id, ack } => (event_id, ack),
        DeliverySendOutcome::Undeliverable { .. } | DeliverySendOutcome::NoAck { .. } => {
            panic!("expected Delivered, got {outcome:?}")
        }
    };
    assert_eq!(ack.receiver, alice.peer_id(), "ack must name the receiver");
    let (ack_channel, ack_cursor) = match ack.outcome {
        DeliveryOutcome::Delivered { channel, cursor } => (channel, cursor),
        DeliveryOutcome::Undeliverable { reason } => {
            panic!("delivered ack must carry channel+cursor, got undeliverable {reason:?}")
        }
    };
    assert_eq!(
        ack_channel, alice_room.channel,
        "delivered ack channel must be the receiver's bound room"
    );
    assert_eq!(
        ack_cursor.event_id, event_id,
        "delivered cursor must point at the acked event"
    );

    // The ack's claim must be true: the event is durably in ALICE's
    // store, queryable through her room transcript surface. (Mutation
    // check: with `store.append` skipped on the receive path, no
    // delivered ack is emitted and this test fails on the match
    // above; with the ack moved before persistence, this read is the
    // backstop.)
    let recent = alice.page_recent(16).await.expect("alice page_recent");
    assert!(
        recent.iter().any(|event| event.event_id == event_id),
        "delivered-acked event must be visible in the receiver's room transcript; got {} events",
        recent.len()
    );
}

#[tokio::test]
async fn send_to_unbound_channel_is_undeliverable_with_loud_receiver_diagnostic() {
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let (alice, bob) = paired_handles(&tmp_a, &tmp_b).await;

    // Receiver-side diagnostics become assertable: inject a memory
    // sink. Suppressing the frame_undeliverable emission makes this
    // test fail (mutation-verified during development).
    let sink = MemoryDiagnosticSink::default();
    alice.set_diagnostic_sink(Arc::new(sink.clone()));

    // Bob binds + sends on a channel alice NEVER binds.
    bob.join("zzz-unbound-39d37629").await.expect("bob joins");

    let alice_addr: SocketAddr = alice
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("alice listens");
    bob.connect_lan(alice_addr, alice.peer_id())
        .await
        .expect("bob dials alice");

    let outcome = bob
        .send_with_delivery_ack(
            "nobody on the receiving side reads this channel",
            Headers::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("ack-requesting send succeeds");

    let ack = match outcome {
        DeliverySendOutcome::Undeliverable { ack, .. } => ack,
        DeliverySendOutcome::Delivered { .. } | DeliverySendOutcome::NoAck { .. } => {
            panic!("expected Undeliverable, got {outcome:?}")
        }
    };
    assert_eq!(
        ack.outcome,
        DeliveryOutcome::Undeliverable {
            reason: UndeliverableReason::UnknownChannel
        },
        "the typed reason must be unknown_channel"
    );

    // The receiver-side diagnostic is part of the contract — silent
    // drops are the bug class under test.
    let undeliverable_diags: Vec<_> = sink
        .events()
        .into_iter()
        .filter(|event| event.code == DiagnosticCode::FrameUndeliverable)
        .collect();
    assert!(
        !undeliverable_diags.is_empty(),
        "receiver MUST emit a frame_undeliverable diagnostic for an unbound-channel frame"
    );
    assert!(
        undeliverable_diags.iter().any(|event| event
            .fields
            .get("reason")
            .is_some_and(|reason| reason == "unknown_channel")),
        "diagnostic must carry reason=unknown_channel; got {undeliverable_diags:?}"
    );
}

#[tokio::test]
async fn receiver_that_accepts_but_never_persists_yields_no_ack() {
    // The "receiver killed mid-send" / old-build wire shape: a
    // TLS-pinned listener that accepts the connection and the frame
    // but has NO ingest/persist/ack path behind it. Built from the
    // raw transport adapter — exactly what the sender's TLS layer
    // sees when the remote daemon's ingest is gone (live repro
    // 2026-06-12 02:36Z: TLS succeeded, frame in no store, sender
    // printed "sent"). The fixed verb must report NoAck, distinct
    // from both delivered and undeliverable.
    use airc_protocol::{PeerKeyRegistry, PeerKeypair};
    use airc_transport::LanTcpAdapter;

    let tmp_b = TempDir::new().expect("bob tempdir");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");
    bob.join("delivery-ack-room").await.expect("bob joins");

    let hollow_id = airc_core::PeerId::from_u128(0x710c_39d3_7629);
    let hollow_kp = PeerKeypair::generate();
    let hollow_pubkey = hollow_kp.public_bytes();

    // Mutual pinning: the hollow listener trusts bob, bob trusts the
    // hollow listener — the handshake succeeds like production.
    let bob_spec: airc_lib::PeerSpec = bob.peer_spec().parse().expect("bob spec");
    let registry = PeerKeyRegistry::new();
    registry
        .enrol(hollow_id, 0, hollow_pubkey)
        .expect("enrol hollow self");
    registry
        .enrol(bob_spec.peer_id, 0, bob_spec.pubkey)
        .expect("enrol bob");
    let hollow = LanTcpAdapter::new(hollow_id, hollow_kp, Arc::new(registry))
        .expect("hollow adapter builds");
    bob.add_peer(airc_lib::PeerSpec {
        peer_id: hollow_id,
        pubkey: hollow_pubkey,
    })
    .await
    .expect("bob trusts hollow listener");

    let hollow_addr: SocketAddr = hollow
        .listen(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("hollow listener binds");
    bob.connect_lan(hollow_addr, hollow_id)
        .await
        .expect("bob dials the hollow listener");

    let outcome = bob
        .send_with_delivery_ack(
            "is anyone actually persisting this?",
            Headers::new(),
            Duration::from_millis(1_500),
        )
        .await
        .expect("send still flushes to the live TLS connection");

    match outcome {
        DeliverySendOutcome::NoAck { waited, .. } => {
            assert_eq!(
                waited,
                Duration::from_millis(1_500),
                "NoAck must report the bounded wait that elapsed"
            );
        }
        DeliverySendOutcome::Delivered { .. } | DeliverySendOutcome::Undeliverable { .. } => {
            panic!("a receiver with no live ingest must yield NoAck, got {outcome:?}")
        }
    }
}
