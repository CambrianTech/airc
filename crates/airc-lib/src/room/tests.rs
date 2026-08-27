use super::*;
use tempfile::TempDir;

#[test]
fn from_name_is_deterministic_across_homes() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();
    let a = Room::from_name(home_a.path(), "project-x").unwrap();
    let b = Room::from_name(home_b.path(), "project-x").unwrap();
    assert_eq!(a.channel, b.channel);
    assert_ne!(a.wire, b.wire);
}

#[test]
fn from_name_differs_per_name() {
    let home = TempDir::new().unwrap();
    let a = Room::from_name(home.path(), "general").unwrap();
    let b = Room::from_name(home.path(), "private").unwrap();
    assert_ne!(a.channel, b.channel);
    assert_ne!(a.wire, b.wire);
}

#[test]
fn sanitise_replaces_path_separators() {
    assert_eq!(sanitise_name("../etc/passwd"), "---etc-passwd");
    assert_eq!(sanitise_name("normal-name_42"), "normal-name_42");
}

#[test]
fn at_channel_adopts_the_given_channel_verbatim() {
    // what this catches: a reply built for the REQUEST's channel must ride
    // that exact channel — re-deriving from the name (which can diverge per
    // identity scope, or be absent) re-creates the blind-room miss that made
    // every cross-scope command dispatch die at the deadline (2026-08-27).
    let home = TempDir::new().unwrap();
    let foreign = RoomId::from_uuid(Uuid::new_v4());
    let room = Room::at_channel(home.path(), "general", foreign).unwrap();
    assert_eq!(room.channel, foreign);
    assert_ne!(
        Room::from_name(home.path(), "general").unwrap().channel,
        foreign
    );
    // Empty name (request had no stamped channel-name header) still yields
    // a usable room on the adopted channel.
    let unnamed = Room::at_channel(home.path(), "", foreign).unwrap();
    assert_eq!(unnamed.channel, foreign);
}
