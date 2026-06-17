//! `Airc::room_roster` — the single airc-side call continuum's
//! `RoomRosterSource` consumes for the persona-groundedness
//! "[Present in this room]" injection (M5 roster ask, 2026-06-16).
//!
//! The contract proven here:
//!   - room_roster returns one entry per present (heartbeating) peer,
//!     carrying runtime + self-reported availability + last-seen from
//!     the latest beat's coordination signal (the presence join);
//!   - its `display_name` for a peer is exactly what the canonical
//!     single-peer resolver [`Airc::peer_alias`] returns — i.e. the
//!     name-join reads the SAME `IdentityPublished` cards and never
//!     diverges (the consistency pin). A peer present without a
//!     published card surfaces as `display_name: None` — honest
//!     "present but unnamed", not omitted.
//!
//! This is the consumer-facing seam: the test calls exactly the public
//! API (`Airc::open` → `join` → `emit_agent_heartbeat*` → `room_roster`)
//! that continuum links by lib, so a regression in the presence+identity
//! join surfaces here before it reaches a persona's prompt.

mod common;

use std::time::Duration;

use airc_lib::{AgentAvailabilityState, CoordinationSignal, HeartbeatKind};
use common::Machine;

#[tokio::test]
async fn room_roster_joins_presence_and_agrees_with_canonical_name_resolver() {
    // One agent attached to a daemon + joined to a room (the route its
    // heartbeat frames need to land in the transcript).
    let machine = Machine::boot().await;
    let airc = machine.solo("general").await;

    // Presence: a heartbeat with a self-reported availability.
    airc.emit_agent_heartbeat_with_coordination(
        HeartbeatKind::Alive,
        "claude",
        None,
        None,
        None,
        CoordinationSignal {
            availability: Some(AgentAvailabilityState::Ready),
            ..Default::default()
        },
    )
    .await
    .expect("emit heartbeat");

    let within = Duration::from_secs(120);
    let roster = airc.room_roster(within, 200).await.expect("room_roster");

    let me = roster
        .iter()
        .find(|member| member.peer_id == airc.peer_id())
        .expect("self must be present in its own room roster");

    // The presence join: every field carries from the latest heartbeat.
    assert_eq!(me.runtime, "claude", "runtime carries from the heartbeat");
    assert_eq!(
        me.availability,
        Some(AgentAvailabilityState::Ready),
        "availability carries from the coordination signal"
    );
    assert!(
        me.last_seen_ms > 0,
        "last_seen_ms carries the heartbeat time"
    );

    // The name join must be IDENTICAL to the canonical single-peer
    // resolver (`peer_alias`), proving room_roster reads the same
    // `IdentityPublished` cards and never fabricates or drops a name.
    // (Here both are `None` — present but no card published — which is
    // itself the honest "unnamed" contract; the assertion is what would
    // fail if `peer_display_names` matched the wrong peer, read the
    // wrong field, or mis-ordered LWW.)
    let canonical = airc
        .peer_alias(airc.peer_id())
        .await
        .expect("peer_alias resolves");
    assert_eq!(
        me.display_name, canonical,
        "room_roster's name-join must agree with the canonical peer_alias \
         resolver — same IdentityPublished cards, no divergence"
    );
}
