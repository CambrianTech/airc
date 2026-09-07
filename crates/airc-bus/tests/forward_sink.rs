//! Card 1998f6cb — the router's outbound route-layer tap
//! (`EventRouter::set_forward_sink`), the OUTBOUND mirror of card
//! 4132f48c's `publish_if_new` ingest.
//!
//! Pins, at the bus level (the airc-lib integration suite covers the
//! full two-daemon TLS path):
//!   1. every successfully published DURABLE envelope reaches the
//!      sink, carrying the origin LAN peer the publish came in with
//!      (`None` for local publishes) — the forwarder's loop-prevention
//!      input;
//!   2. a `Duplicate` outcome of `publish_if_new` NEVER reaches the
//!      sink — re-arrivals are dead-ends, which is what makes mesh
//!      forwarding terminate;
//!   3. `EphemeralLatest` envelopes ARE offered, carrying their origin
//!      (card bf4d4556 — capacity offers are ephemeral, and the old
//!      "never offered" rule is what left the capacity plane dark on
//!      every node; stream classes remain machine-local);
//!   4. sink saturation neither blocks nor fails the publish hot path,
//!      and its accounting is class-dependent: a durable overflow is a
//!      counted LOUD drop (data loss), an ephemeral overflow is a
//!      counted benign SUPERSEDE (latest-wins — the next offer carries
//!      the same truth).

mod common;

use std::time::Duration;

use airc_bus::{ForwardItem, PublishIfNew, RouterConfig};
use airc_core::{PeerId, RoomId};
use common::{durable, ephemeral, Owner};
use tokio::sync::mpsc;

async fn recv_item(rx: &mut mpsc::Receiver<ForwardItem>) -> ForwardItem {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("forward sink must receive within 2s")
        .expect("forward sink channel must stay open")
}

#[tokio::test]
async fn published_durables_reach_the_forward_sink_with_their_origin() {
    let owner = Owner::new(RouterConfig::default());
    let (tx, mut rx) = mpsc::channel(16);
    owner.router.set_forward_sink(tx);
    let channel = RoomId::from_u128(0xf0);
    let origin_peer = PeerId::from_u128(0xa11ce);

    // Local publish (the IPC `Send` path) → origin None.
    owner
        .router
        .publish(durable(channel, 1, "local send"))
        .await
        .expect("publish");
    let local = recv_item(&mut rx).await;
    assert_eq!(local.env.event_id, airc_core::EventId::from_u128(1));
    assert_eq!(
        local.origin, None,
        "a locally originated publish must carry no origin link"
    );

    // Bridged inbound publish → origin Some(link peer).
    let outcome = owner
        .router
        .publish_if_new_from(durable(channel, 2, "bridged inbound"), Some(origin_peer))
        .await
        .expect("publish_if_new_from");
    assert!(matches!(outcome, PublishIfNew::Published(_)));
    let bridged = recv_item(&mut rx).await;
    assert_eq!(bridged.env.event_id, airc_core::EventId::from_u128(2));
    assert_eq!(
        bridged.origin,
        Some(origin_peer),
        "a bridged publish must carry the link peer it arrived from \
         (loop-prevention input for the forwarder)"
    );
}

#[tokio::test]
async fn duplicates_never_reach_the_forward_sink() {
    // what this catches: re-arrivals must be dead-ends. This is the half
    // of the old `duplicates_and_ephemerals_...` test that did NOT change
    // under card bf4d4556 — it is load-bearing loop termination, and mesh
    // forwarding stops terminating without it.
    let owner = Owner::new(RouterConfig::default());
    let (tx, mut rx) = mpsc::channel(16);
    owner.router.set_forward_sink(tx);
    let channel = RoomId::from_u128(0xf1);
    let origin = PeerId::from_u128(0xb0b);

    owner
        .router
        .publish_if_new_from(durable(channel, 7, "first arrival"), Some(origin))
        .await
        .expect("first publish");
    let first = recv_item(&mut rx).await;
    assert_eq!(first.env.event_id, airc_core::EventId::from_u128(7));

    // Re-arrival of the same event (echo) — Duplicate, NOT re-offered.
    let echo = owner
        .router
        .publish_if_new_from(durable(channel, 7, "first arrival"), Some(origin))
        .await
        .expect("echo publish");
    assert_eq!(echo, PublishIfNew::Duplicate);

    let extra = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(
        extra.is_err(),
        "a Duplicate re-arrival may never reach the forward sink; got {extra:?}"
    );
}

#[tokio::test]
async fn ephemerals_reach_the_forward_sink_so_capacity_offers_cross_the_wire() {
    // what this catches: regression for card bf4d4556 — the capacity
    // plane dark on all three grid nodes. Capacity offers publish as
    // `EphemeralLatest`; the offer to the forward sink used to sit inside
    // the `is_durable()` block, so they never left the machine and every
    // node heard only its own echo. This test is the inversion of the old
    // "ephemerals never offered" assertion, which pinned that bug as if
    // it were the design.
    let owner = Owner::new(RouterConfig::default());
    let (tx, mut rx) = mpsc::channel(16);
    owner.router.set_forward_sink(tx);
    let channel = RoomId::from_u128(0xf3);
    let origin_peer = PeerId::from_u128(0xcafe);

    owner
        .router
        .publish(ephemeral(channel, 8, "capacity", b"xy"))
        .await
        .expect("ephemeral publish");
    let local = recv_item(&mut rx).await;
    assert_eq!(local.env.event_id, airc_core::EventId::from_u128(8));
    assert_eq!(
        local.origin, None,
        "a locally originated ephemeral must carry no origin link"
    );

    // A bridged ephemeral must carry its origin too — without it the
    // forwarder cannot apply loop prevention and an offer would echo
    // back over the link it arrived on.
    owner
        .router
        .publish_if_new_from(ephemeral(channel, 9, "capacity", b"zz"), Some(origin_peer))
        .await
        .expect("bridged ephemeral publish");
    let bridged = recv_item(&mut rx).await;
    assert_eq!(bridged.env.event_id, airc_core::EventId::from_u128(9));
    assert_eq!(
        bridged.origin,
        Some(origin_peer),
        "a bridged ephemeral must carry the link peer it arrived from, or \
         the forwarder will echo it back over that same link"
    );
}

#[tokio::test]
async fn ephemeral_saturation_is_a_benign_supersede_not_a_loud_drop() {
    // what this catches: card bf4d4556's second half. Once ephemerals are
    // forwarded they can also overflow the tap — and for latest-wins
    // traffic that is NOT data loss, because the next offer carries the
    // same truth. If these were counted as `forward_drop_count` the grid
    // would raise a data-loss alarm at the exact moment the design is
    // working. Supersede-on-full IS the coalescing for this class, which
    // is why no latest-map is needed.
    let owner = Owner::new(RouterConfig::default());
    let (tx, _rx) = mpsc::channel(1);
    owner.router.set_forward_sink(tx);
    let channel = RoomId::from_u128(0xf4);

    for marker in 0..4u128 {
        owner
            .router
            .publish(ephemeral(channel, 200 + marker, "capacity", b"q"))
            .await
            .expect("publish must keep succeeding while the forward tap overflows");
    }

    assert_eq!(
        owner.router.forward_drop_count(),
        0,
        "a superseded ephemeral is not durable data loss and must never be \
         counted as a loud drop"
    );
    assert_eq!(
        owner.router.ephemeral_superseded_count(),
        3,
        "superseded ephemerals must still be COUNTED — benign is not the \
         same as invisible, and a pathological offer rate has to be visible"
    );
}

#[tokio::test]
async fn forward_sink_saturation_is_a_counted_loud_drop_not_a_publish_failure() {
    let owner = Owner::new(RouterConfig::default());
    // Capacity 1 and never drained: every publish past the first must
    // overflow the tap.
    let (tx, _rx) = mpsc::channel(1);
    owner.router.set_forward_sink(tx);
    let channel = RoomId::from_u128(0xf2);

    for marker in 0..4u128 {
        owner
            .router
            .publish(durable(channel, 100 + marker, "burst"))
            .await
            .expect("publish must keep succeeding while the forward tap overflows");
    }

    assert_eq!(
        owner.router.forward_drop_count(),
        3,
        "every overflowed offer must be counted (loud-drop, never silent)"
    );
}
