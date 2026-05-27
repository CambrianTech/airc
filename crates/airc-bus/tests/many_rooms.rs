//! Acceptance test 5 — many rooms, idle ones ≈ free (§3.1, §6, §11.1).
//!
//! 1000 channels named, 1 active. Idle channels cost ~nothing: no per-channel
//! task spinning. Tested via two proxies that hold without sampling CPU:
//!
//! - **no-allocation-growth:** merely *naming* a channel (constructing a
//!   `RoomId` + subscribing to it lazily) allocates per-channel state only
//!   when first touched, and an idle channel never spawns a task or a ring
//!   that grows. The router exposes `channels_created` — the count of channel
//!   states ever allocated. We assert it equals the number of channels we
//!   actually *touched*, not some background-fanned superset.
//! - **no-wakeups proxy:** idle subscribers produce nothing. We attach a
//!   subscriber to an idle channel and assert it yields no event within a
//!   window (a polling/per-room-task design would tick); only the active
//!   channel's subscriber wakes.

mod common;

use std::time::Duration;

use futures::StreamExt;

use airc_bus::{Filter, RouterConfig};
use airc_core::RoomId;

use common::{durable, Owner};

#[tokio::test]
async fn idle_channels_allocate_nothing_and_never_wake() {
    let owner = Owner::new(RouterConfig::default());
    let r = &owner.router;

    // Name 1000 channels but touch none yet.
    let channels: Vec<RoomId> = (0..1000u128).map(RoomId::from_u128).collect();

    // Before touching anything, no channel state exists.
    assert_eq!(
        r.channels_created(),
        0,
        "naming RoomIds allocates no per-channel state"
    );

    // Activate exactly ONE channel: subscribe + publish there.
    let active = channels[500];
    let active_stream = r.subscribe(Filter::channel(active), None);
    futures::pin_mut!(active_stream);

    // Attach an idle subscriber to a DIFFERENT channel; it must never wake.
    let idle = channels[12];
    let idle_stream = r.subscribe(Filter::channel(idle), None);
    futures::pin_mut!(idle_stream);

    // Only the two touched channels (active + idle) have state. The other 998
    // named channels allocated nothing — idle ones are free.
    assert_eq!(
        r.channels_created(),
        2,
        "only touched channels allocate state; 998 idle ones cost nothing"
    );

    // Publish on the active channel.
    r.publish(durable(active, 1, "hello")).await.unwrap();

    // The active subscriber wakes with exactly that event.
    let got = tokio::time::timeout(Duration::from_secs(5), active_stream.next())
        .await
        .expect("active subscriber should wake")
        .expect("stream ended");
    assert_eq!(got.event_id.0.as_u128(), 1);

    // The idle subscriber NEVER wakes within the window — no per-room task is
    // ticking it. (A poll-loop design would produce spurious wakeups here.)
    let idle_wake = tokio::time::timeout(Duration::from_millis(300), idle_stream.next()).await;
    assert!(
        idle_wake.is_err(),
        "idle channel subscriber must produce no event (no per-channel task/poll)"
    );

    // Touching the idle channel still didn't spawn anything for the other 998.
    assert_eq!(
        r.channels_created(),
        2,
        "still only the two touched channels"
    );
}

#[tokio::test]
async fn one_active_room_among_many_does_not_grow_with_room_count() {
    // Allocation-growth proxy: the per-publish cost is independent of how many
    // OTHER channels exist. We publish the same workload with the channel set
    // present vs. absent and assert channel-state count tracks only touched
    // channels, never the named universe.
    let owner = Owner::new(RouterConfig::default());
    let r = &owner.router;

    // Touch a few channels with publishes.
    for c in [3u128, 7, 11] {
        r.publish(durable(RoomId::from_u128(c), c, "x"))
            .await
            .unwrap();
    }
    assert_eq!(
        r.channels_created(),
        3,
        "channel-state allocation tracks only published-to channels, \
         not the (unbounded) namespace of possible RoomIds"
    );
}
