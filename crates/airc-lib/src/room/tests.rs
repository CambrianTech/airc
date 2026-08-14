use super::*;
use tempfile::TempDir;

#[test]
fn mint_gives_every_room_its_own_id() {
    // what this catches: an id derived from the name. Two rooms carrying
    // the SAME label are two rooms — the label is not the identity, and
    // minting the same id for both is the ghost-room collision that a
    // v5(name) derivation made unavoidable.
    let home = TempDir::new().unwrap();
    let a = Room::mint(home.path(), "project-x").unwrap();
    let b = Room::mint(home.path(), "project-x").unwrap();
    assert_ne!(a.channel, b.channel, "each mint is its own room");
    assert_ne!(a.wire, b.wire, "and its own bytes");
    assert_eq!(a.name, b.name, "sharing a label is legal");
}

#[test]
fn mint_ids_are_v4_out_of_the_callers_control() {
    // what this catches: any reintroduction of a derived id. A v5 uuid
    // here means something hashed an input, and whatever it hashed just
    // became the room's identity.
    let home = TempDir::new().unwrap();
    let room = Room::mint(home.path(), "general").unwrap();
    assert_eq!(
        room.channel.as_uuid().get_version(),
        Some(uuid::Version::Random)
    );
}

#[test]
fn wire_dir_is_keyed_by_id_so_no_name_can_reach_the_filesystem() {
    // what this catches: `wires/<sanitised-name>`. A path-traversal name
    // used to decide where a room's bytes live, which is why a sanitiser
    // existed at all. The id is always a valid single path component.
    let home = TempDir::new().unwrap();
    let room = Room::mint(home.path(), "../etc/passwd").unwrap();
    assert_eq!(
        room.wire,
        home.path().join("wires").join(room.channel.to_string())
    );
}

#[test]
fn name_is_a_label_that_addresses_nothing() {
    // what this catches: re-coupling the label to the address. Renaming
    // must be an edit to one field and nothing else.
    let home = TempDir::new().unwrap();
    let mut room = Room::mint(home.path(), "general").unwrap();
    let (id, wire) = (room.channel, room.wire.clone());
    room.name = "renamed".to_string();
    assert_eq!(room.channel, id);
    assert_eq!(room.wire, wire);
}
