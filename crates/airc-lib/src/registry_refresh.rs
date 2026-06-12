//! Daemon-resident account-registry publish/refresh loop — the
//! keystone that turns the already-built account-registry machinery
//! into ZERO-human-action cross-machine discovery (card
//! a134b370-10b1-49c6-aa42-e1a05446e887).
//!
//! ## What was unwired
//!
//! `Airc::publish_account_registry` / `refresh_account_registry`
//! (account_registry.rs) and `GhAccountRegistryStore`
//! (gh_account_registry.rs) were both complete and tested, but
//! NOTHING called them on a cadence — only unit tests invoked them.
//! Two machines on the same GitHub account therefore never converged
//! without a human running a manual courier step. This module is the
//! missing cadence.
//!
//! ## Loop shape (mirrors `airc-daemon::server::run`)
//!
//! - First tick fires ~10s after daemon start (let transports settle,
//!   avoid a publish storm on a flapping process).
//! - Subsequent ticks every `cadence` (default 120s). gist writes are
//!   rate-limited by GitHub (per-hour create/update budget), and a
//!   registry beacon that lags peer discovery by ≤2min is well inside
//!   the human-coordination latency this replaces. 120s keeps us far
//!   from any abuse threshold while still feeling instant relative to
//!   the months of manual coordination it deletes.
//! - Completion-to-start sequencing: a single tick runs
//!   publish-then-refresh to completion before the next interval is
//!   armed (`tokio::time::interval` with `MissedTickBehavior::Delay`),
//!   so a slow gh round-trip never overlaps itself.
//! - Shutdown: ONE pinned `Notified`-style future held across the
//!   whole loop (same discipline as `server::run`) so a
//!   `notify_waiters` can't be lost in the window between iterations.
//!
//! ## gh-auth gate
//!
//! The gh gist store is the same-account rendezvous transport. Like
//! every airc transport it is OPTIONAL: if `gh` is not authenticated
//! we emit exactly ONE diagnostic per tick and skip — no crash loop,
//! no repeated spew beyond the per-tick cadence. A Sqlite/in-memory
//! store needs no gate (`RegistryRefreshGate::Always`), which is what
//! the acceptance test uses to prove the wiring without real gh auth.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use tokio::time::{interval_at, Instant, MissedTickBehavior};

use crate::account_registry::AccountRegistryStore;
use crate::Airc;

/// Default delay before the first publish/refresh after daemon start.
pub const DEFAULT_FIRST_TICK: Duration = Duration::from_secs(10);

/// Default cadence between ticks. See module docs for the 120s rationale.
pub const DEFAULT_CADENCE: Duration = Duration::from_secs(120);

/// Timing knobs for the registry-refresh loop. Defaults are the
/// production cadence; tests shrink them so a bounded run completes
/// fast (hazard d2ba719c — all waits bounded).
#[derive(Debug, Clone, Copy)]
pub struct RegistryRefreshConfig {
    pub first_tick: Duration,
    pub cadence: Duration,
}

impl Default for RegistryRefreshConfig {
    fn default() -> Self {
        Self {
            first_tick: DEFAULT_FIRST_TICK,
            cadence: DEFAULT_CADENCE,
        }
    }
}

/// Whether (and how) to gate a tick on transport availability.
#[derive(Debug, Clone)]
pub enum RegistryRefreshGate {
    /// gh gist rendezvous: skip the tick unless the hermetic gate
    /// (card d793c242) allows this scope to touch gh AND `gh auth
    /// status` is ready. `gh_bin` is the gh binary path override
    /// (`None` = `gh` on PATH); `scope_home` is the publishing
    /// scope's AIRC_HOME (the hermetic gate's input). This is the
    /// production gate for `GhAccountRegistryStore`.
    GhAuth {
        gh_bin: Option<PathBuf>,
        scope_home: PathBuf,
    },
    /// No gate — always run the tick. Used by Sqlite/in-memory stores
    /// that need no external auth (acceptance test + manual `registry
    /// sync` against a local store).
    Always,
}

/// Why a tick was skipped. Both variants are clean skips, NOT errors —
/// but they are LOUD skips: every caller logs the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateBlock {
    /// Hermetic gate (card d793c242): this scope must never touch the
    /// gh account rendezvous. The payload says exactly why.
    Hermetic(crate::gh_account_registry::AccountRegistryBlock),
    /// `gh` is not authenticated — the optional same-account
    /// rendezvous transport is simply unavailable.
    GhNotReady,
}

impl std::fmt::Display for GateBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hermetic(block) => write!(f, "{block}"),
            Self::GhNotReady => write!(
                f,
                "gh transport not ready (not authenticated); this is the optional \
                 same-account rendezvous — skipping cleanly"
            ),
        }
    }
}

impl RegistryRefreshGate {
    /// Returns `None` if a tick should proceed, or the reason it must
    /// not. The hermetic gate is checked FIRST: a hermetic scope must
    /// not even probe `gh auth status`.
    async fn block(&self) -> Option<GateBlock> {
        match self {
            RegistryRefreshGate::GhAuth { gh_bin, scope_home } => {
                if let Some(block) = crate::gh_account_registry::account_registry_block(scope_home)
                {
                    return Some(GateBlock::Hermetic(block));
                }
                if !crate::gh_account_registry::gh_auth_ready(gh_bin.as_deref()).await {
                    return Some(GateBlock::GhNotReady);
                }
                None
            }
            RegistryRefreshGate::Always => None,
        }
    }
}

/// Run ONE publish+refresh tick. Returns the count of peers enrolled
/// from the refreshed document (0 if the store had no document yet).
/// On any failure, emits a typed diagnostic and returns the error so
/// callers (the CLI verb) can surface it; the loop logs-and-continues.
async fn run_tick(
    airc: &Airc,
    store: &dyn AccountRegistryStore,
    sink: &dyn DiagnosticSink,
) -> Result<TickReport, crate::error::AircError> {
    let published = match airc.publish_account_registry(store).await {
        Ok(doc) => doc,
        Err(error) => {
            sink.emit(
                DiagnosticEvent::error(
                    DiagnosticComponent::Daemon,
                    DiagnosticCode::AccountRegistryPublishFailed,
                    "account registry publish failed",
                )
                .with_field("error", &error),
            );
            return Err(error);
        }
    };
    let refreshed = match airc.refresh_account_registry(store).await {
        Ok(doc) => doc,
        Err(error) => {
            sink.emit(
                DiagnosticEvent::error(
                    DiagnosticComponent::Daemon,
                    DiagnosticCode::AccountRegistryRefreshFailed,
                    "account registry refresh failed",
                )
                .with_field("error", &error),
            );
            return Err(error);
        }
    };
    let enrolled = refreshed.as_ref().map(|doc| doc.peers.len()).unwrap_or(0);
    Ok(TickReport {
        published_peers: published.peers.len(),
        enrolled_peers: enrolled,
    })
}

/// One-tick outcome, surfaced by the `registry sync` CLI verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickReport {
    /// Peers carried in the document this node published.
    pub published_peers: usize,
    /// Peers present in the merged document refreshed back from the
    /// store (the set this node enrolled / re-confirmed).
    pub enrolled_peers: usize,
}

/// Outcome of [`sync_once`]: the tick either ran, or was skipped for
/// a typed, printable reason. The reason is part of the contract —
/// callers MUST surface it (no silent skips, card d793c242).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    Ran(TickReport),
    Skipped(GateBlock),
}

/// Run exactly one publish+refresh, honoring the gate. Used by the
/// `registry sync` CLI verb (manual proof + Mac bootstrap surface) and
/// composable into the loop. A gate-skip returns
/// `Ok(SyncOutcome::Skipped(reason))`.
pub async fn sync_once(
    airc: &Airc,
    store: &dyn AccountRegistryStore,
    gate: &RegistryRefreshGate,
) -> Result<SyncOutcome, crate::error::AircError> {
    if let Some(block) = gate.block().await {
        return Ok(SyncOutcome::Skipped(block));
    }
    let report = run_tick(airc, store, &StderrJsonDiagnosticSink).await?;
    Ok(SyncOutcome::Ran(report))
}

/// The daemon-resident loop. Ticks publish+refresh on a cadence until
/// `shutdown` resolves. Each tick:
///   1. checks the gate (one diagnostic + skip if the transport isn't
///      ready — no crash loop);
///   2. publishes this node's account-registry document;
///   3. refreshes (fetch + import) — auto-enrolling same-account peers.
///
/// Failures are logged via typed diagnostics and the loop continues —
/// this is satellite-grade umbilical infra (self-heal, no human in the
/// loop). `shutdown` is awaited via a single pinned future so a
/// `notify_waiters()` landing between ticks is never lost (the exact
/// hazard `server::run` documents).
pub async fn run_loop<S>(
    airc: Airc,
    store: S,
    gate: RegistryRefreshGate,
    config: RegistryRefreshConfig,
    shutdown: impl Future<Output = ()>,
) where
    S: AccountRegistryStore,
{
    let sink = StderrJsonDiagnosticSink;
    let mut ticker = interval_at(Instant::now() + config.first_tick, config.cadence);
    // Completion-to-start: if a tick runs long, delay the next tick a
    // full cadence from completion rather than firing immediately to
    // "catch up" (which would risk back-to-back gist writes).
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            _ = ticker.tick() => {
                if let Some(block) = gate.block().await {
                    let code = match &block {
                        GateBlock::Hermetic(_) => DiagnosticCode::AccountRegistryHermeticSkip,
                        GateBlock::GhNotReady => DiagnosticCode::AccountRegistryPublishFailed,
                    };
                    sink.emit(
                        DiagnosticEvent::warn(
                            DiagnosticComponent::Daemon,
                            code,
                            format!("account registry tick skipped: {block}"),
                        ),
                    );
                    continue;
                }
                // run_tick already emits typed diagnostics on failure;
                // the loop deliberately swallows the Err and keeps
                // ticking (self-heal: a transient gh failure must not
                // kill discovery for the daemon's lifetime).
                let _ = run_tick(&airc, &store, &sink).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_registry::SqliteAccountRegistryStore;
    use crate::route::RouteEndpoint;
    use std::sync::Arc;
    use tempfile::tempdir;

    async fn write_identity(home: &std::path::Path) {
        let store = airc_store::SqliteEventStore::open_path(&home.join("events.sqlite"))
            .await
            .unwrap();
        crate::mesh_identity::resolve_with(
            &store,
            || {
                Some((
                    "joelteply".to_string(),
                    crate::mesh_identity::Source::Operator,
                ))
            },
            4_102_444_800_000,
        )
        .await
        .unwrap();
    }

    async fn sqlite_registry_store_at(dir: &std::path::Path) -> SqliteAccountRegistryStore {
        let path = dir.join("events.sqlite");
        let event_store = airc_store::SqliteEventStore::open_path(&path)
            .await
            .unwrap();
        SqliteAccountRegistryStore::new(Arc::new(event_store))
    }

    // THE ACCEPTANCE TEST (zero-manual-enrol proof).
    //
    // Two Airc handles, SAME mesh identity (joelteply), SEPARATE
    // tempdir homes + SEPARATE wire roots, bridged ONLY by a shared
    // Sqlite AccountRegistryStore (NOT the gh store — needs no real gh
    // auth, so the proof stands even while every other machine is
    // dark). Handle A publishes its endpoints; handle B refreshes and
    // is asserted to have auto-enrolled A as a trusted peer AND stored
    // A's endpoints — with ZERO manual add_peer anywhere in the test.
    // Driven through `sync_once` (the same entry the CLI verb + loop
    // use) with `RegistryRefreshGate::Always`.
    #[tokio::test]
    async fn sync_once_auto_enrols_same_account_peer_zero_manual_add() {
        let dir = tempdir().unwrap();
        let machine_a = dir.path().join("machine-a/.airc");
        let machine_b = dir.path().join("machine-b/.airc");
        let wire_a = dir.path().join("wire-a");
        let wire_b = dir.path().join("wire-b");
        write_identity(&wire_a).await;
        write_identity(&wire_b).await;
        let store = sqlite_registry_store_at(&dir.path().join("rendezvous")).await;

        let airc_a = Airc::open_with_wire_root_for_test(&machine_a, &wire_a)
            .await
            .unwrap();
        airc_a.join("general").await.unwrap();
        // Give A a routable endpoint so the beacon carries something to
        // store (proves endpoints survive the bridge, not just the key).
        airc_a
            .upsert_route_endpoint(RouteEndpoint::Relay {
                url: "https://relay.example.test".to_string(),
            })
            .unwrap();

        let airc_b = Airc::open_with_wire_root_for_test(&machine_b, &wire_b)
            .await
            .unwrap();

        // A runs the FULL wired tick (publish + refresh) through the
        // same `sync_once` the daemon loop and CLI verb call. This
        // proves the wired publish lands A's beacon on the rendezvous.
        //
        // NOTE on store choice: the SqliteAccountRegistryStore keys ONE
        // row per mesh identity (A and B share `joelteply`), so it
        // models a SINGLE shared rendezvous slot — if B also published
        // it would clobber A's row before reading it. The PRODUCTION
        // rendezvous (`GhAccountRegistryStore`) sidesteps this with
        // per-MACHINE gists that the reader merges, so both directions
        // converge there. For this network-free proof we therefore
        // drive A's publish and B's REFRESH (the import path that does
        // the auto-enrol) — exactly the bridge the existing
        // `sqlite_registry_bridges_two_isolated_machine_homes` test
        // models, now asserted through the wired `sync_once` entry.
        let gate = RegistryRefreshGate::Always;
        let a_report = match sync_once(&airc_a, &store, &gate).await.unwrap() {
            SyncOutcome::Ran(report) => report,
            SyncOutcome::Skipped(block) => panic!("gate Always must run A's tick: {block}"),
        };
        assert!(
            a_report.published_peers >= 1,
            "A must publish at least its own beacon"
        );

        // B refreshes from the rendezvous — auto-enrolling A with ZERO
        // manual add_peer. This is the keystone behaviour.
        let refreshed = airc_b.refresh_account_registry(&store).await.unwrap();
        assert!(
            refreshed.is_some(),
            "B must find A's published document on the rendezvous"
        );

        // PROOF 1: B trusts A's peer key with no manual add_peer.
        let trusted = airc_trust::load(airc_b.wire_root()).await.unwrap();
        assert!(
            trusted.iter().any(|p| p.peer_id == airc_a.peer_id()),
            "B must auto-enrol A as a trusted peer via the registry — zero manual add"
        );

        // PROOF 2: B stored A's endpoints (the route can form post-restart).
        let a_record = trusted
            .iter()
            .find(|p| p.peer_id == airc_a.peer_id())
            .expect("A present in B's trust store");
        let endpoints_json = a_record
            .endpoints_json
            .as_deref()
            .expect("A's endpoints must be persisted on B's trust record");
        assert!(
            endpoints_json.contains("relay.example.test"),
            "B must have stored A's relay endpoint, got: {endpoints_json}"
        );

        // PROOF 3: B's coordinator now sees A's presence beacon (the
        // route-discovery input), again with zero manual action.
        let snapshot = crate::coordinator::snapshot_store(
            airc_b.coordinator_store(),
            &crate::subscriptions::MeshIdentity::new("joelteply"),
            &Default::default(),
            u64::MAX,
        )
        .await
        .unwrap();
        assert!(
            snapshot
                .stale
                .iter()
                .chain(snapshot.live.iter())
                .any(|p| p.peer_id == airc_a.peer_id()),
            "B's coordinator must carry A's beacon after auto-discovery"
        );
    }

    // The gate cleanly skips (returns Ok(None)) without touching the
    // store when the transport isn't ready — no crash, no error.
    #[tokio::test]
    async fn sync_once_skips_when_gate_not_ready() {
        let dir = tempdir().unwrap();
        let machine = dir.path().join("machine/.airc");
        let wire = dir.path().join("wire");
        write_identity(&wire).await;
        let store = sqlite_registry_store_at(&dir.path().join("rendezvous")).await;
        let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
            .await
            .unwrap();

        // A gh binary that does not exist → gh_auth_ready is false →
        // gate skips. Bounded (gh_auth_ready has its own timeout).
        // scope_home is production-shaped so the HERMETIC gate does
        // not fire first — this test pins the auth-skip arm.
        let gate = RegistryRefreshGate::GhAuth {
            gh_bin: Some(PathBuf::from("airc-nonexistent-gh-binary-for-gate-test")),
            scope_home: PathBuf::from("/machine/prod/.airc"),
        };
        let outcome = sync_once(&airc, &store, &gate).await.unwrap();
        assert_eq!(
            outcome,
            SyncOutcome::Skipped(GateBlock::GhNotReady),
            "unready gate must skip with the gh-not-ready reason"
        );
    }

    // HERMETIC GATE (card d793c242): a gh-gated scope whose home is
    // temp-rooted must skip the tick with the hermetic reason — gh is
    // never probed, nothing is published. Mutation check: removing the
    // hermetic arm from `RegistryRefreshGate::block` makes this fall
    // through to GhNotReady (or run), failing the assert.
    #[tokio::test]
    async fn sync_once_skips_hermetically_for_temp_scope_home() {
        let dir = tempdir().unwrap();
        let machine = dir.path().join("machine/.airc");
        let wire = dir.path().join("wire");
        write_identity(&wire).await;
        let store = sqlite_registry_store_at(&dir.path().join("rendezvous")).await;
        let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
            .await
            .unwrap();

        let gate = RegistryRefreshGate::GhAuth {
            gh_bin: Some(PathBuf::from("airc-nonexistent-gh-binary-for-gate-test")),
            scope_home: machine.clone(),
        };
        match sync_once(&airc, &store, &gate).await.unwrap() {
            SyncOutcome::Skipped(GateBlock::Hermetic(
                crate::gh_account_registry::AccountRegistryBlock::TempScopeHome { scope_home },
            )) => assert_eq!(scope_home, machine),
            other => panic!("temp-rooted scope must skip hermetically, got {other:?}"),
        }
    }

    // The loop honors shutdown promptly even with a long cadence —
    // proves the pinned-shutdown wiring (bounded by the 1s test budget).
    #[tokio::test]
    async fn run_loop_exits_on_shutdown() {
        let dir = tempdir().unwrap();
        let machine = dir.path().join("machine/.airc");
        let wire = dir.path().join("wire");
        write_identity(&wire).await;
        let store = sqlite_registry_store_at(&dir.path().join("rendezvous")).await;
        let airc = Airc::open_with_wire_root_for_test(&machine, &wire)
            .await
            .unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let config = RegistryRefreshConfig {
            // Tick almost immediately so the loop body runs at least once.
            first_tick: Duration::from_millis(10),
            cadence: Duration::from_secs(3600),
        };
        let handle = tokio::spawn(async move {
            run_loop(
                airc,
                store,
                RegistryRefreshGate::Always,
                config,
                async move {
                    let _ = rx.await;
                },
            )
            .await;
        });
        // Let one tick land, then signal shutdown and assert the loop
        // returns inside a bounded window.
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("loop must exit promptly on shutdown")
            .unwrap();
    }
}
