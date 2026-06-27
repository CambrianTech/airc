//! Regression for the #1217 cross-review (M5): `airc network` advertised
//! a "no mutation" contract but read its default room via
//! `Airc::current_room`, which on a fresh scope LAZILY subscribes to
//! #general, sets it default, persists it, and publishes a presence
//! beacon. Merely *inspecting* the mesh must never mutate it.
//!
//! `Airc::peek_default_room` is the read-only accessor the inspection
//! path now uses. This pins the invariant: on a fresh scope it returns `None`
//! and persists NOTHING — so a second handle on the same home still sees
//! no default room. (Against `current_room`, the reopen would find the
//! lazily-created #general subscription, failing the assert.)

use airc_lib::Airc;
use tempfile::tempdir;

#[tokio::test]
async fn default_room_is_read_only_and_persists_nothing_on_fresh_scope() {
    let dir = tempdir().unwrap();
    let machine = dir.path().join("machine/.airc");
    let wire = dir.path().join("wire");

    let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("open fresh scope");

    // Fresh scope has no default room — and asking for it must not
    // create one.
    assert!(
        airc.peek_default_room()
            .await
            .expect("default_room")
            .is_none(),
        "fresh scope must have no default room"
    );

    // The load-bearing assertion: nothing was persisted. A brand-new
    // handle on the SAME home still sees no default room. If
    // `default_room` had gone through `current_room` (subscribe +
    // set_default + save), this reopened handle would find #general.
    drop(airc);
    let reopened = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("reopen same home");
    assert!(
        reopened
            .peek_default_room()
            .await
            .expect("default_room after reopen")
            .is_none(),
        "default_room must not persist a subscription — read-only contract (#1217)"
    );
}
