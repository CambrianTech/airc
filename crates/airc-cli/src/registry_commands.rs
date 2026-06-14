//! `airc registry sync` — manual one-shot of the account-registry
//! publish/refresh the daemon otherwise runs on a cadence (keystone
//! card a134b370-10b1-49c6-aa42-e1a05446e887). Bootstraps a fresh
//! machine and proves same-account discovery on demand.
//!
//! ## Dialable-endpoint requirement (card 4b6a0ffa / #33)
//!
//! This verb opens its own short-lived `Airc` handle with no listener,
//! so its `route_endpoints()` is empty. It used to publish that
//! anyway, OVERWRITING this machine's registry gist with an
//! endpoint-less self-beacon — a fresh same-account reader then got
//! enrol-but-never-route (#33's exact shape, narrowed to
//! first-contact-after-manual-sync). Binding a listener here would be
//! WORSE: the advertised port dies with this process.
//!
//! The fix: read back the DAEMON's live endpoints over typed IPC
//! (`Request::RouteEndpoints`) and publish those. If no daemon is
//! running — or it has no endpoints — REFUSE the endpoint-less
//! overwrite with an actionable message, unless the operator
//! explicitly passes `--allow-endpointless`. No silent endpoint-less
//! publishes.

use std::path::Path;

use airc_ipc::{DaemonClient, IpcRouteEndpoint};
use airc_lib::{
    machine_account_home, Airc, GateBlock, GcAction, GhAccountRegistryStore, RegistryRefreshGate,
    RouteEndpoint, SyncOutcome,
};
use airc_store::SqliteEventStore;
use std::sync::Arc;

/// Convert the daemon's IPC endpoint mirror back to the lib type.
/// Explicit per-variant mapping — this crate is the only one that
/// sees both types, and the exhaustive match means a new variant on
/// either side is a compile error here, not silent drift.
pub(crate) fn route_endpoint_from_ipc(endpoint: IpcRouteEndpoint) -> RouteEndpoint {
    match endpoint {
        IpcRouteEndpoint::LanTcp { addr } => RouteEndpoint::LanTcp { addr },
        IpcRouteEndpoint::TailscaleTcp { addr } => RouteEndpoint::TailscaleTcp { addr },
        IpcRouteEndpoint::Udp { addr } => RouteEndpoint::Udp { addr },
        IpcRouteEndpoint::Relay { url } => RouteEndpoint::Relay { url },
        IpcRouteEndpoint::Reticulum { destination } => RouteEndpoint::Reticulum { destination },
        IpcRouteEndpoint::WebRtcSignaling { url } => RouteEndpoint::WebRtcSignaling { url },
    }
}

/// Inverse of [`route_endpoint_from_ipc`] — what the daemon's registry
/// glue records into `DaemonState.route_endpoints` after binding its
/// listener.
pub(crate) fn route_endpoint_to_ipc(endpoint: RouteEndpoint) -> IpcRouteEndpoint {
    match endpoint {
        RouteEndpoint::LanTcp { addr } => IpcRouteEndpoint::LanTcp { addr },
        RouteEndpoint::TailscaleTcp { addr } => IpcRouteEndpoint::TailscaleTcp { addr },
        RouteEndpoint::Udp { addr } => IpcRouteEndpoint::Udp { addr },
        RouteEndpoint::Relay { url } => IpcRouteEndpoint::Relay { url },
        RouteEndpoint::Reticulum { destination } => IpcRouteEndpoint::Reticulum { destination },
        RouteEndpoint::WebRtcSignaling { url } => IpcRouteEndpoint::WebRtcSignaling { url },
    }
}

/// The endpoint decision for a manual publish, separated from I/O so
/// the refusal policy is unit-testable (and mutation-verifiable).
///
/// - `handle_endpoints`: this process's own `route_endpoints()`
///   (normally empty for the CLI — no listener);
/// - `daemon_endpoints`: `Ok(endpoints)` from the daemon probe, or
///   `Err(reason)` when no daemon answered (not running, or too old
///   to speak the verb);
/// - `allow_endpointless`: the explicit operator override.
///
/// Returns the endpoints to publish, or `Err` with the loud,
/// actionable refusal text. NO arm returns publishable-empty unless
/// the operator named it.
pub(crate) fn resolve_publish_endpoints(
    handle_endpoints: Vec<RouteEndpoint>,
    daemon_endpoints: Result<Vec<RouteEndpoint>, String>,
    allow_endpointless: bool,
) -> Result<Vec<RouteEndpoint>, String> {
    if !handle_endpoints.is_empty() {
        return Ok(handle_endpoints);
    }
    let daemon_reason = match daemon_endpoints {
        Ok(endpoints) if !endpoints.is_empty() => return Ok(endpoints),
        Ok(_) => "the daemon is running but advertises no endpoints \
                  (its LAN listener may have failed to bind — check the \
                  daemon's stderr)"
            .to_string(),
        Err(reason) => format!("no daemon answered the endpoint probe: {reason}"),
    };
    if allow_endpointless {
        return Ok(Vec::new());
    }
    Err(format!(
        "registry sync REFUSED: this machine has no dialable endpoint to publish.\n  \
         {daemon_reason}\n  \
         Publishing anyway would OVERWRITE this machine's registry gist with an \
         endpoint-less self-beacon, so same-account peers could enrol this key but \
         never route to it (#33).\n  \
         Fix: start the daemon (`airc join` starts it) so its live endpoints can be \
         read back and published, or re-run with --allow-endpointless to publish a \
         key-only beacon on purpose."
    ))
}

/// Probe the daemon that owns this home's socket for its advertised
/// endpoints. `Err(reason)` covers every can't-answer shape: no
/// daemon, stale pre-verb daemon, RPC failure.
async fn daemon_route_endpoints(home: &Path) -> Result<Vec<RouteEndpoint>, String> {
    let socket = crate::cli::default_socket_path_in(home);
    let client = DaemonClient::new(socket.clone());
    match client.route_endpoints().await {
        Ok(response) => Ok(response
            .endpoints
            .into_iter()
            .map(route_endpoint_from_ipc)
            .collect()),
        Err(error) => Err(format!("{error} (socket: {})", socket.display())),
    }
}

pub async fn run_sync(
    home: &Path,
    allow_endpointless: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;

    // The gh sentinel + registry rows live in the machine-account
    // home's events.sqlite — the same DB the daemon's store uses.
    let db_path = machine_account_home(home).join("events.sqlite");
    let event_store = Arc::new(SqliteEventStore::open_path(&db_path).await?);
    let store = GhAccountRegistryStore::new(event_store, home);
    let gate = RegistryRefreshGate::GhAuth {
        // Honor the operator's AIRC_GH_BIN here too (card 1f2cbffa):
        // the store's `new()` already reads it, but the gate used to
        // probe bare `gh` — an override pointing at a different gh
        // would pass/fail the gate against the WRONG binary. `None`
        // token slot: a manual sync runs in the operator's live
        // session, where env/keyring are already current.
        gh_bin: crate::commands::gh_bin_override(),
        scope_home: home.to_path_buf(),
        token_override: None,
    };

    // Gate FIRST (hermetic, then gh-auth): a blocked scope must hear
    // the gate's reason, not an endpoint refusal it can't act on.
    if let Some(block) = gate.block().await {
        print_gate_skip("sync", &block);
        return Ok(());
    }

    // Card 4b6a0ffa / #33: a manual sync must publish a DIALABLE
    // endpoint — the daemon's live one, read back over typed IPC —
    // or refuse. Never silently endpoint-less.
    let publish_endpoints = resolve_publish_endpoints(
        airc.route_endpoints()?,
        daemon_route_endpoints(home).await,
        allow_endpointless,
    )?;
    if publish_endpoints.is_empty() {
        println!(
            "registry sync: publishing WITHOUT endpoints (--allow-endpointless): \
             same-account peers can enrol this key but cannot dial this machine."
        );
    }
    for endpoint in publish_endpoints {
        airc.upsert_route_endpoint(endpoint)?;
    }

    match airc_lib::registry_sync_once(&airc, &store, &gate).await? {
        SyncOutcome::Ran(report) => {
            println!(
                "registry sync ok: published {} peer(s) to the account rendezvous; \
                 enrolled {} peer(s) from the merged registry.",
                report.published_peers, report.enrolled_peers
            );
            if report.enrolled_peers <= 1 {
                println!(
                    "  (only this node is visible so far — run `airc registry sync` on \
                     another machine signed into the same GitHub account to converge.)"
                );
            }
        }
        // The gate re-checks inside sync_once; if it flipped between
        // our check and the tick, surface the reason the same way.
        SyncOutcome::Skipped(block) => print_gate_skip("sync", &block),
    }
    Ok(())
}

fn print_gate_skip(verb: &str, block: &GateBlock) {
    match block {
        // HERMETIC GATE (card d793c242): this scope must not touch the
        // gh account rendezvous — say exactly why, loudly. The manual
        // verb honors the same gate as the daemon loop; there is no
        // CLI path around it (the gh store enforces it again inside).
        GateBlock::Hermetic(reason) => {
            println!("registry {verb} REFUSED (hermetic gate): {reason}");
        }
        GateBlock::GhNotReady => {
            println!(
                "registry {verb} skipped: `gh` is not authenticated. The account-mesh \
                 rendezvous is the same-account gist transport — sign in with `gh auth \
                 login` to enable zero-touch cross-machine discovery. (This is optional, \
                 like every airc transport; LAN/relay/Reticulum paths are unaffected.)"
            );
        }
    }
}

/// `airc registry gc` — prune junk registry gists on this account.
/// Dry-run by default; `--apply` deletes. Cuts the per-convergence gh
/// fetch cost back down to one gist per real machine.
pub async fn run_gc(home: &Path, apply: bool) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = machine_account_home(home).join("events.sqlite");
    let event_store = Arc::new(SqliteEventStore::open_path(&db_path).await?);
    let store = GhAccountRegistryStore::new(event_store, home);
    let gate = RegistryRefreshGate::GhAuth {
        gh_bin: crate::commands::gh_bin_override(),
        scope_home: home.to_path_buf(),
        token_override: None,
    };
    if let Some(block) = gate.block().await {
        print_gate_skip("gc", &block);
        return Ok(());
    }

    let report = store.gc(apply).await?;
    for verdict in &report.verdicts {
        let tag = match verdict.action {
            GcAction::Delete => "DELETE",
            GcAction::Keep => "keep  ",
        };
        let short = &verdict.id[..verdict.id.len().min(8)];
        println!(
            "  {tag}  {short}  {} — {}",
            verdict.filename, verdict.reason
        );
    }
    if report.applied {
        println!(
            "registry gc: deleted {} junk gist(s), kept {} real gist(s).",
            report.deleted, report.kept
        );
        if report.deleted < report.to_delete {
            println!(
                "  ({} delete(s) failed — see errors above; re-run to retry.)",
                report.to_delete - report.deleted
            );
        }
    } else if report.to_delete == 0 {
        println!(
            "registry gc: clean — {} real gist(s), nothing to delete.",
            report.kept
        );
    } else {
        println!(
            "registry gc (dry run): would delete {} junk gist(s), keep {} real gist(s). \
             Re-run with --apply to delete.",
            report.to_delete, report.kept
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn lan(port: u16) -> RouteEndpoint {
        RouteEndpoint::LanTcp {
            addr: SocketAddr::from(([10, 0, 0, 2], port)),
        }
    }

    // Card 4b6a0ffa / #33 — the refusal policy, pinned. Mutation
    // check (verified at authoring time): making the no-endpoint arm
    // return Ok(Vec::new()) instead of refusing fails
    // `refuses_endpointless_publish_without_override`.
    #[test]
    fn refuses_endpointless_publish_without_override() {
        let refusal =
            resolve_publish_endpoints(Vec::new(), Err("daemon not reachable".to_string()), false)
                .expect_err("no endpoints anywhere must refuse");
        assert!(refusal.contains("REFUSED"), "loud refusal, got: {refusal}");
        assert!(
            refusal.contains("--allow-endpointless"),
            "actionable override named, got: {refusal}"
        );
        assert!(
            refusal.contains("daemon not reachable"),
            "probe failure reason surfaced, got: {refusal}"
        );
    }

    #[test]
    fn refuses_when_daemon_is_up_but_endpointless() {
        let refusal = resolve_publish_endpoints(Vec::new(), Ok(Vec::new()), false)
            .expect_err("a non-dialable daemon must also refuse");
        assert!(refusal.contains("advertises no endpoints"), "{refusal}");
    }

    #[test]
    fn publishes_daemon_endpoints_when_available() {
        let endpoints = resolve_publish_endpoints(Vec::new(), Ok(vec![lan(7717)]), false)
            .expect("daemon endpoints must be publishable");
        assert_eq!(endpoints, vec![lan(7717)]);
    }

    #[test]
    fn own_handle_endpoints_win_over_daemon_probe() {
        let endpoints = resolve_publish_endpoints(
            vec![lan(1)],
            Err("irrelevant — handle already dialable".to_string()),
            false,
        )
        .expect("handle endpoints must be publishable");
        assert_eq!(endpoints, vec![lan(1)]);
    }

    #[test]
    fn explicit_override_allows_endpointless_publish() {
        let endpoints =
            resolve_publish_endpoints(Vec::new(), Err("daemon not reachable".to_string()), true)
                .expect("the named override must allow a key-only beacon");
        assert!(endpoints.is_empty());
    }

    // The IPC mirror conversion is total and lossless in both
    // directions — a new RouteEndpoint variant breaks this at compile
    // time (exhaustive matches), and a field mix-up breaks it here.
    #[test]
    fn ipc_endpoint_conversion_round_trips_every_variant() {
        let endpoints = vec![
            RouteEndpoint::LanTcp {
                addr: SocketAddr::from(([10, 0, 0, 2], 7717)),
            },
            RouteEndpoint::TailscaleTcp {
                addr: SocketAddr::from(([100, 64, 0, 7], 7717)),
            },
            RouteEndpoint::Udp {
                addr: SocketAddr::from(([10, 0, 0, 2], 7718)),
            },
            RouteEndpoint::Relay {
                url: "https://relay.example.test".to_string(),
            },
            RouteEndpoint::Reticulum {
                destination: "abcdef0123456789".to_string(),
            },
            RouteEndpoint::WebRtcSignaling {
                url: "wss://signal.example.test".to_string(),
            },
        ];
        for endpoint in endpoints {
            assert_eq!(
                route_endpoint_from_ipc(route_endpoint_to_ipc(endpoint.clone())),
                endpoint
            );
        }
    }
}
