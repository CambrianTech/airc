//! Rendezvous: how two scopes that have never met end up in ONE room.
//!
//! A room label keys a room only WITHIN one account. Across two
//! accounts it keys nothing — so "we both joined #project" is not a
//! shared room, it is two rooms wearing the same word, and every
//! frame between them comes back `Undeliverable { UnknownChannel }`.
//!
//! The only thing that crosses an account boundary intact is the id.
//! An `InviteBeacon` carries the ids of the rooms its publisher is in
//! (`rooms: Vec<RoomId>` — ids, never `(id, label)` pairs, because a
//! pair makes the pair the unit of exchange and re-imports naming
//! into the rendezvous). The recipient imports the beacon, reads the
//! ids, and joins one by id.
//!
//! This file pins that chain end to end, and pins the negative half
//! too: joining by the LABEL instead gets you a different room. That
//! second assertion is the whole reason the by-id verb exists.

use airc_lib::Airc;
use tempfile::TempDir;

/// The rendezvous keystone: beacon → import → id → join, and both
/// scopes land in the SAME room.
///
/// what this catches: a beacon that advertises reachability but not
/// rooms (the shape before `rooms` existed), leaving a freshly-paired
/// peer able to reach its partner and with no way to name anywhere to
/// speak; and any regression where `join_room_id` resolves through
/// the label instead of taking the id it was handed.
#[tokio::test]
async fn a_peer_joins_the_publishers_room_by_id_from_its_invite_beacon() {
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let alice = Airc::open(tmp_a.path().join(".airc"))
        .await
        .expect("alice open");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");

    // Alice is in a room. Her account minted the id; the label is
    // hers, and stays hers.
    let alice_room = alice.join("design-review").await.expect("alice joins");

    // The beacon carries the ids of the rooms she is in.
    let beacon = alice
        .invite_beacon_with_rooms()
        .await
        .expect("alice builds a beacon carrying her rooms");
    assert!(
        beacon.rooms.contains(&alice_room.channel),
        "the beacon must advertise the room its publisher is in; \
         without it a paired peer has reachability and nowhere to go. \
         advertised: {:?}, alice is in: {}",
        beacon.rooms,
        alice_room.channel
    );

    bob.import_invite_beacon(beacon)
        .await
        .expect("bob imports alice's invite");

    // What bob can now join, BY ID. Nothing here is resolved by name.
    let joinable = bob.peer_room_ids().expect("bob reads peer room ids");
    assert!(
        joinable.contains(&alice_room.channel),
        "an imported invite's rooms must surface as joinable ids; \
         joinable: {joinable:?}, expected to contain {}",
        alice_room.channel
    );

    // Bob joins by id, labelling it whatever he likes — the label is
    // local display and deliberately NOT alice's word for the room.
    let bob_room = bob
        .join_room_id(alice_room.channel, "the-thing-alice-invited-me-to")
        .await
        .expect("bob joins by id");

    assert_eq!(
        bob_room.channel, alice_room.channel,
        "both scopes must be in ONE room — the id is the room"
    );
    assert_ne!(
        bob_room.name, alice_room.name,
        "and they may call it different things: a label is per-scope \
         display, so a matching id with differing labels is the \
         contract working, not a bug"
    );
}

/// The negative half, and the reason the by-id verb had to exist.
///
/// what this catches: any return of name-derived room identity. If a
/// label ever keys a room across accounts again, these two ids
/// collide and this test fails — which is exactly the silent
/// `UnknownChannel` bug, caught at build time instead of on the wire.
#[tokio::test]
async fn joining_the_same_label_on_another_account_is_a_different_room() {
    let tmp_a = TempDir::new().expect("alice tempdir");
    let tmp_b = TempDir::new().expect("bob tempdir");
    let alice = Airc::open(tmp_a.path().join(".airc"))
        .await
        .expect("alice open");
    let bob = Airc::open(tmp_b.path().join(".airc"))
        .await
        .expect("bob open");

    let alice_room = alice.join("design-review").await.expect("alice joins");
    let bob_room = bob.join("design-review").await.expect("bob joins");

    assert_ne!(
        alice_room.channel, bob_room.channel,
        "two accounts typing the same word must get two DIFFERENT \
         rooms — a label keys nothing across an account boundary. If \
         these are equal, name-as-identity is back and every \
         cross-account send is about to fail as UnknownChannel."
    );
}
