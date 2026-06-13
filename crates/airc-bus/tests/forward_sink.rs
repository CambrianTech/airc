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
//!   3. ephemeral classes stay machine-local (never offered);
//!   4. sink saturation is a counted loud-drop that neither blocks
//!      nor fails the publish hot path.

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
async fn duplicates_and_ephemerals_never_reach_the_forward_sink() {
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

    // Ephemeral — machine-local in this slice, never offered.
    owner
        .router
        .publish(ephemeral(channel, 8, "pose", b"xy"))
        .await
        .expect("ephemeral publish");

    let extra = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(
        extra.is_err(),
        "neither a Duplicate re-arrival nor an ephemeral may reach the \
         forward sink; got {extra:?}"
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
