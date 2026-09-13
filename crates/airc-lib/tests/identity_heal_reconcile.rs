//! `Airc::reconcile_subscription_identity` — the AUTONOMOUS half of the
//! self-healing join.
//!
//! The failure this closes, in full: a machine that boots before `gh` can
//! answer resolves the provisional `local:<host>:<user>` mesh identity and
//! derives every room UUID under it. Room UUIDs are
//! `UUIDv5(mesh_identity ‖ NUL ‖ channel_name)`, so that node's `#general`
//! is a DIFFERENT room from the account's real `#general` — it is invisible
//! to its own peers while reporting a healthy join. Later, once `gh`
//! answers, the identity heals; the stored subscriptions do NOT, so the
//! scope keeps reading dead diverged rooms.
//!
//! `SubscriptionSet::rebind_diverged` was already the correct heal, but it
//! only ran on the JOIN path — so recovery required a human typing
//! `airc stop && airc join`. That manual ritual IS the recurring "airc is
//! broken again". This reconcile runs the same heal on the daemon's
//! route-refresh clock.
//!
//! What this catches (three things a unit test on `rebind_diverged` cannot):
//!   1. the heal is PERSISTED — a reload from the store sees the converged
//!      UUID, so the daemon (a separate process reading the same store)
//!      actually benefits;
//!   2. it is idempotent — a converged scope reports zero rebinds, so the
//!      per-tick call is free and cannot churn beacons in steady state;
//!   3. name / wire / join time survive the rebind — the channel NAME is the
//!      durable identity; only the derived UUID moves.

use airc_lib::mesh_identity::{self, CachedIdentity, Source};
use airc_lib::subscriptions::{self, ChannelName, MeshIdentity};
use airc_lib::Airc;
use tempfile::TempDir;

/// Pin an identity the resolver will honor verbatim: `Source::Operator`
/// entries are explicit overrides — trusted as-is, never TTL-expired, never
/// re-resolved — so the test never shells out to `gh` and never depends on
/// the host's GitHub auth state.
///
/// Safe on a real developer machine: `machine_account_home` treats a
/// temp-rooted scope as its OWN account boundary, so this coordinator store
/// is the tempdir's, never `~/.airc`.
async fn pin_identity(airc: &Airc, identity: &str) {
    mesh_identity::save(
        airc.coordinator_store_for_test(),
        &CachedIdentity {
            version: 1,
            identity: identity.to_string(),
            source: Source::Operator,
            resolved_at_ms: 1,
            // The store persists this as a signed integer, so it must stay in
            // i64 range. Value is irrelevant to the test: `Operator` entries
            // early-return from `resolve` before any expiry check.
            ttl_ms: i64::MAX as u64,
        },
    )
    .await
    .expect("pin mesh identity");
}

#[tokio::test]
async fn identity_heal_reconcile_persists_and_is_idempotent() {
    let tmp = TempDir::new().expect("tempdir");
    let airc = Airc::open(tmp.path().join(".airc")).await.expect("open");

    // Boot under the provisional identity a `gh`-less boot mints.
    pin_identity(&airc, "local:unknown-host:unknown-user").await;
    airc.join("general").await.expect("join general");

    let general = ChannelName::new("general").expect("channel name");
    let diverged = MeshIdentity::new("local:unknown-host:unknown-user");
    let healed = MeshIdentity::new("joelteply");

    let stored_before = airc
        .subscriptions()
        .await
        .expect("load before")
        .into_iter()
        .find(|sub| sub.name == general)
        .expect("general subscribed");
    assert_eq!(
        stored_before.room_id,
        subscriptions::derive_room_id(&diverged, &general),
        "the join derived the room UUID under the provisional identity",
    );

    // `gh` answers; the machine-wide identity heals.
    pin_identity(&airc, "joelteply").await;

    let rebinds = airc
        .reconcile_subscription_identity()
        .await
        .expect("reconcile after heal");
    assert_eq!(rebinds.len(), 1, "the diverged subscription must re-bind");
    assert_eq!(rebinds[0].old_room_id, stored_before.room_id);
    assert_eq!(
        rebinds[0].new_room_id,
        subscriptions::derive_room_id(&healed, &general),
    );

    // (1) PERSISTED: a fresh load — what the daemon process sees — carries
    // the converged UUID. Without the store write the heal would evaporate
    // and the node would stay invisible to its own mesh forever.
    let stored_after = airc
        .subscriptions()
        .await
        .expect("load after")
        .into_iter()
        .find(|sub| sub.name == general)
        .expect("still subscribed");
    assert_eq!(
        stored_after.room_id,
        subscriptions::derive_room_id(&healed, &general),
        "the converged room UUID must be durable, not in-memory only",
    );

    // (3) the channel NAME is the durable identity — the rest must not move.
    assert_eq!(stored_after.name, stored_before.name);
    assert_eq!(stored_after.wire, stored_before.wire);
    assert_eq!(stored_after.joined_at_ms, stored_before.joined_at_ms);

    // (2) IDEMPOTENT: the per-tick call on a converged scope is a no-op.
    let again = airc
        .reconcile_subscription_identity()
        .await
        .expect("reconcile when converged");
    assert!(
        again.is_empty(),
        "a converged scope must report no rebinds so the daemon tick is free",
    );
}
