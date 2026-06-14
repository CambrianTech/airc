//! Regression baseline + investigation pin: sibling scopes on one machine.
//!
//! Card 326000a5. Field repro on canary 80a407b0aaa1 (Intel Mac):
//!   1. Main scope: `/Users/joel/Development/continuum/.airc`, peer 9bb24964, room #general
//!   2. Sibling scope: `AIRC_HOME=/tmp/airc-b airc init` + `airc room general`, peer 671e7e28
//!   3. Cross-enrol via `airc peer add` both directions
//!   4. Sibling: `airc msg "..."` reports 'sent to general — 1 paired peer(s) + any local scope tailing this channel.'
//!   5. Main scope's `airc events list --kind message` does NOT contain the message.
//!
//! ## What these tests pin
//!
//! Both tests below CURRENTLY PASS on canary. That's the investigation pin:
//! the substrate library delivers correctly through both the shared-mesh-root
//! convergence path AND through independent wire roots attached to one
//! daemon, as long as the test wires the scopes to the same `DaemonFixture`
//! socket directly. So the field-repro bug is NOT in the SDK send/route/
//! subscribe layer — it lies somewhere in the CLI plumbing that operators
//! actually hit:
//!
//!   - socket-path resolution per cwd / per `AIRC_HOME` (do two `airc`
//!     invocations from different homes end up at the SAME daemon socket,
//!     or do they each spin up their own?)
//!   - `ensure_daemon_running` lifecycle when `AIRC_HOME` overrides
//!   - `airc events list` reading from a per-home transcript store that the
//!     sibling's send never landed in, even though the broker fan-out
//!     succeeded
//!
//! Field-symptom diff to look for in the daemon log: scope-b's send fires the
//! broker, but scope-a is registered against a different daemon process /
//! different db path, so the fan-out has no peer-local subscriber to deliver
//! to.
//!
//! Treat these tests as the lower-bound proof — when a fix lands for the
//! CLI-side resolution, an end-to-end CLI test in `airc-cli/tests/` should
//! pair off these to lock in the fix at both layers.

mod common;

use std::time::Duration;

use airc_lib::Airc;
use common::{trust, DaemonFixture, Machine};
use tempfile::TempDir;

/// Baseline: with the shared-mesh-root convergence path (the test fixture's
/// canonical "two tabs under one $HOME" shape), intra-machine sibling-scope
/// delivery works. Passes on canary today.
#[tokio::test]
async fn shared_mesh_root_two_scopes_deliver() {
    let machine = Machine::boot().await;
    let (alice, bob) = machine.pair_in("sibling-scope-routing-shared").await;

    let body = "sibling-scope-routing-326000a5-shared";
    alice.say(body).await.expect("alice sends");
    tokio::time::sleep(Duration::from_millis(250)).await;

    let bob_events = bob.page_recent(50).await.expect("bob reads transcript");
    let found = bob_events.iter().any(|ev| {
        ev.body
            .as_ref()
            .and_then(|b| b.as_text())
            .map(|t| t.contains(body))
            .unwrap_or(false)
    });
    assert!(
        found,
        "shared-mesh-root convergence regressed: bob did not receive alice's message"
    );
}

/// Field repro: two scopes whose AIRC_HOME values live OUTSIDE a shared mesh
/// root (`AIRC_HOME=/tmp/airc-b airc init` next to `~/.airc/`). They each
/// resolve their own wire root, independent identity keypairs, but attach to
/// the SAME daemon socket — exactly what happens in production when an
/// operator runs `AIRC_HOME=/tmp/airc-b airc join` alongside `airc join` in
/// their main home.
///
/// On canary 80a407b0aaa1 the operator observes:
/// > sent to general (5eedf7b1-...) — 1 paired peer(s) + any local scope
/// > tailing this channel.
///
/// …yet the sibling scope's `page_recent` / `events list` does NOT see the
/// message. This test reproduces that field gap inside the SDK.
#[tokio::test]
async fn independent_wire_roots_two_scopes_share_one_daemon() {
    let daemon = DaemonFixture::start().await;

    let root_a = TempDir::new().expect("root a");
    let root_b = TempDir::new().expect("root b");
    let home_a = root_a.path().join("alice");
    let home_b = root_b.path().join("bob");

    let alice =
        Airc::attach_with_wire_root_for_test(home_a, root_a.path().to_path_buf(), &daemon.socket)
            .await
            .expect("alice attaches to shared daemon");

    let bob =
        Airc::attach_with_wire_root_for_test(home_b, root_b.path().to_path_buf(), &daemon.socket)
            .await
            .expect("bob attaches to shared daemon");

    trust(&alice, &bob).await;

    let room = "sibling-scope-routing-independent";
    alice.join(room).await.expect("alice joins");
    bob.join(room).await.expect("bob joins");

    let body = "sibling-scope-routing-326000a5-independent";
    alice.say(body).await.expect("alice sends");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let bob_events = bob.page_recent(50).await.expect("bob reads transcript");
    let found = bob_events.iter().any(|ev| {
        ev.body
            .as_ref()
            .and_then(|b| b.as_text())
            .map(|t| t.contains(body))
            .unwrap_or(false)
    });

    assert!(
        found,
        "sibling-scope routing gap (card 326000a5): two scopes with independent wire roots, sharing one daemon socket, mutually trusted, both joined to the SAME room name — bob received {} events; none matched body={body:?}. Field repro: AIRC_HOME=/tmp/airc-b airc msg ... reports 'sent + 1 paired peer' but the main scope's events list never sees it.",
        bob_events.len()
    );
}
