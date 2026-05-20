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
fn load_or_default_returns_default_when_missing() {
    let home = TempDir::new().unwrap();
    let room = load_or_default(home.path()).unwrap();
    assert_eq!(room.name, "default");
}

#[test]
fn save_then_load_roundtrips() {
    let home = TempDir::new().unwrap();
    let original = Room::from_name(home.path(), "test-room").unwrap();
    save(home.path(), &original).unwrap();
    let loaded = load_or_default(home.path()).unwrap();
    assert_eq!(loaded.name, original.name);
    assert_eq!(loaded.channel, original.channel);
    assert_eq!(loaded.wire, original.wire);
}

#[test]
fn sanitise_replaces_path_separators() {
    assert_eq!(sanitise_name("../etc/passwd"), "---etc-passwd");
    assert_eq!(sanitise_name("normal-name_42"), "normal-name_42");
}

#[test]
fn refuses_unknown_schema_version() {
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path()).unwrap();
    std::fs::write(
        path_in(home.path()),
        r#"{"version":999,"name":"x","wire":"/tmp","channel":"00000000-0000-0000-0000-000000000000","joined_at_ms":0}"#,
    )
    .unwrap();
    let result = load_or_default(home.path());
    assert!(matches!(
        result,
        Err(RoomError::SchemaVersionMismatch { found: 999, .. })
    ));
}
