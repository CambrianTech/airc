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
