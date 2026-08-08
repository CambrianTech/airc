//! `Airc::current_room_landing_in` — a hosting substrate chooses where a
//! FRESH scope's first subscription lands, instead of inheriting airc's
//! `#general` lobby and migrating afterwards (the lazy subscribe is a real
//! attach point — presence + identity card — so a wrong first landing is
//! visible noise, not a free move). Continuum's citizens land in
//! `#academy` (card daa01102); airc's own `current_room` keeps `#general`.
//!
//! Pins three invariants:
//! 1. A fresh scope lands in the CALLER-CHOSEN room, durably (a reopened
//!    handle sees the same default).
//! 2. An ESTABLISHED scope ignores `landing` — the durable default wins.
//! 3. `current_room()` still lands fresh scopes in `#general` (no drift
//!    for every existing caller).

use airc_lib::Airc;
use tempfile::tempdir;

#[tokio::test]
async fn fresh_scope_lands_in_caller_chosen_room_durably() {
    let dir = tempdir().unwrap();
    let machine = dir.path().join("machine/.airc");
    let wire = dir.path().join("wire");

    let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("open fresh scope");

    let room = airc
        .current_room_landing_in("academy")
        .await
        .expect("land fresh scope");
    assert_eq!(
        room.name, "academy",
        "fresh scope must land in the caller-chosen room, not #general"
    );

    // Durable: a new handle on the same home resolves the same default
    // WITHOUT being told a landing room (current_room's #general landing
    // must not fire — the scope is established now).
    drop(airc);
    let reopened = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("reopen same home");
    let default = reopened.current_room().await.expect("read default room");
    assert_eq!(
        default.name, "academy",
        "the chosen landing room must persist as the durable default"
    );
}

#[tokio::test]
async fn established_scope_ignores_landing_param() {
    let dir = tempdir().unwrap();
    let machine = dir.path().join("machine/.airc");
    let wire = dir.path().join("wire");

    let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("open fresh scope");

    airc.current_room_landing_in("academy")
        .await
        .expect("land fresh scope");

    // A later caller naming a DIFFERENT landing room must get the
    // established default back — `landing` only decides the first
    // subscription, never a migration.
    let room = airc
        .current_room_landing_in("somewhere-else")
        .await
        .expect("read established scope");
    assert_eq!(
        room.name, "academy",
        "landing param must be inert on an established scope"
    );
}

#[tokio::test]
async fn current_room_still_lands_fresh_scopes_in_general() {
    let dir = tempdir().unwrap();
    let machine = dir.path().join("machine/.airc");
    let wire = dir.path().join("wire");

    let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
        .await
        .expect("open fresh scope");

    let room = airc.current_room().await.expect("default room");
    assert_eq!(
        room.name, "general",
        "current_room's fresh-scope landing must stay #general — \
         every existing caller depends on it"
    );
}
