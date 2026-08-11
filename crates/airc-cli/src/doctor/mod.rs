//! `airc doctor` — install + identity + scope self-diagnosis with
//! optional auto-recovery.
//!
//! The skill documents agents calling this; the binary owns the
//! diagnostic walk. Each check returns a [`Finding`] with a status
//! (`Ok`, `Info`, `Warn`, `Blocked`) plus the exact one-liner fix
//! the operator (or agent) should run. With `--fix`, doctor applies
//! the safe auto-recoveries inline.
//!
//! ## Layout
//!
//! This module owns only the shared vocabulary ([`Status`], [`Finding`]),
//! the walk ([`run`]) and the auto-recovery pass. Every check lives in its
//! own submodule so a check is read, tested and changed in isolation:
//!
//! | Submodule | Check |
//! |---|---|
//! | [`identity`] | `identity.key` + `local_identity` row pairing |
//! | [`daemon`] | daemon liveness, and (under `--health`) daemon-vs-binary build drift |
//! | [`binary`] | installed binary vs source checkout, DIRECTIONAL via git ancestry |
//! | [`diagnostics`] | recent `DiagnosticEvent`s on the wire |
//! | [`health`] | route/transport health snapshot (`--health`) |
//! | [`delivery`] | delivery truth — whether frames are actually confirmed |
//!
//! ## The recurring bug class these checks exist to avoid
//!
//! Every check here has, at some point, reported `[ok]` on a state it
//! could not actually observe — a status that reports INTENT instead of
//! OUTCOME, or a predicate whose true-branch is unreachable while the
//! node is doing its job:
//!
//! - `[ok]` keyed on how OLD the last ack was, on a route that had been
//!   swallowing traffic for 10 hours (#1318 — now keyed on outstanding
//!   unconfirmed frames).
//! - "installed binary drifted" on a binary that was CURRENT, because the
//!   comparison was `!=` and carried no direction (see [`binary`]).
//! - "no recent diagnostic events" on a busy node, because higher-volume
//!   traffic had evicted every diagnostic from the scan window (see
//!   [`diagnostics`]).
//! - An empty delivery ledger reported as ok, when an empty ledger means
//!   nothing was ever ATTEMPTED (see [`delivery`]).
//!
//! When adding a check, ask the question that catches all of these:
//! **can this `[ok]` be produced by a node that is broken, or by one I
//! simply cannot see?** If yes, it is not an `[ok]`.

mod binary;
mod daemon;
mod delivery;
mod diagnostics;
mod health;
mod identity;

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Info,
    Warn,
    Blocked,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "[ok]",
            Status::Info => "[info]",
            Status::Warn => "[WARN]",
            Status::Blocked => "[BLOCKED]",
        }
    }
}

pub struct Finding {
    pub status: Status,
    pub check: &'static str,
    pub detail: String,
    pub fix: Option<String>,
}

impl Finding {
    fn ok(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status: Status::Ok,
            check,
            detail: detail.into(),
            fix: None,
        }
    }
    fn info(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status: Status::Info,
            check,
            detail: detail.into(),
            fix: None,
        }
    }
    fn warn(check: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            status: Status::Warn,
            check,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
    fn blocked(check: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            status: Status::Blocked,
            check,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// 12-char short form for compact display, matching `airc status`'s
/// `build:` rendering. Shared by the binary-drift and daemon-build checks
/// so the two can never disagree on how a sha is rendered.
fn short_sha(sha: &str) -> String {
    sha[..sha.len().min(12)].to_string()
}

/// How expensive a check is, and therefore when it runs.
///
/// This is the ONLY thing that decided `--health` gating before, and it was
/// encoded as an `if health { ... }` block wrapping two hardcoded calls in
/// [`run`]. Now it is a property each check declares about ITSELF, so adding
/// an expensive check cannot accidentally land in the always-on path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckTier {
    /// Cheap and local: filesystem probes, one 250ms IPC ping. Always runs.
    Always,
    /// Expensive: opens the substrate, re-runs route discovery, dials peers,
    /// queries the daemon's ledger. Only under `--health`.
    Health,
}

/// What a check declares about itself. Mirrors continuum-core's
/// `ServiceModule::config() -> ModuleConfig`: ONE descriptor returned once at
/// registration time, rather than a scatter of metadata methods. Keeping the
/// two traits the same shape is the point — a reader who knows one recognises
/// the other.
pub struct CheckConfig {
    /// Stable identifier, for ordering and diagnostics. Not the
    /// `Finding.check` label — one check may emit findings under several.
    pub name: &'static str,
    pub tier: CheckTier,
}

impl CheckConfig {
    /// Cheap enough to run on every `airc doctor`.
    pub fn always(name: &'static str) -> Self {
        Self {
            name,
            tier: CheckTier::Always,
        }
    }
    /// Expensive — only under `--health`.
    pub fn health(name: &'static str) -> Self {
        Self {
            name,
            tier: CheckTier::Health,
        }
    }
}

/// Everything a check is allowed to depend on. A struct rather than a bare
/// `&Path` so adding shared context later (a pre-opened `Airc` handle, a
/// clock) does not touch seven signatures — the same reason
/// `ServiceModule::initialize` takes a `ModuleContext`.
pub struct CheckContext<'a> {
    pub home: &'a Path,
}

/// One diagnostic concern.
///
/// Deliberately the same shape as continuum-core's `ServiceModule`: a
/// `config()` descriptor + async behavior + `Send + Sync`. Rust has no
/// inheritance, so the analog of an inherited default is a trait DEFAULT
/// METHOD — `ServiceModule::handle_event` carries its auto-route body that
/// way, and here [`CheckConfig::always`] plays the same role: a check states
/// its tier by construction instead of every impl restating it.
///
/// The payoff is that [`run`] no longer names any individual check. It walks
/// [`registry`] and asks each one what it is and when it runs — so a new
/// check is one `impl` plus one registry line, and cannot be half-wired by
/// forgetting to add a call in the middle of the walk. That half-wiring is
/// the same failure mode as the built-but-never-called `note_real_decode`
/// in serving.
#[async_trait::async_trait]
pub trait Check: Send + Sync {
    fn config(&self) -> CheckConfig;

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding>;
}

/// The diagnostic walk, in report order.
///
/// Cheap checks first so a broken scope is named before we spend time dialing
/// peers; the `Health` tier is filtered out entirely unless `--health`.
fn registry() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(identity::IdentityCheck),
        Box::new(daemon::DaemonLivenessCheck),
        Box::new(binary::BinaryFreshnessCheck),
        Box::new(diagnostics::DiagnosticsCheck),
        Box::new(daemon::DaemonBuildCheck),
        Box::new(health::RouteHealthCheck),
        Box::new(delivery::DeliveryTruthCheck),
    ]
}

pub async fn run(home: &Path, fix: bool, health: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("airc doctor — scope: {}", home.display());
    println!();

    let mut applied = Vec::new();
    let mut findings = Vec::new();

    let ctx = CheckContext { home };
    for check in registry() {
        let config = check.config();
        if config.tier == CheckTier::Health && !health {
            continue;
        }
        let before = findings.len();
        findings.extend(check.run(&ctx).await);
        if findings.len() == before {
            // A check that RAN but said nothing is indistinguishable from one
            // that never ran — which is precisely the class of invisibility
            // this module keeps getting bitten by (the delivery-stats Err arm
            // returned an empty Vec and doctor printed `ok (N clean)`). Every
            // registered check accounts for itself, by name.
            findings.push(Finding::info(
                config.name,
                "check ran but reported nothing — no evidence either way",
            ));
        }
    }

    for finding in &findings {
        println!(
            "{} {}: {}",
            finding.status.label(),
            finding.check,
            finding.detail
        );
        if let Some(fix_cmd) = &finding.fix {
            println!("    Fix: {fix_cmd}");
        }
    }

    println!();

    if fix {
        applied.extend(apply_fixes(home, &findings).await?);
    }

    let degraded = findings
        .iter()
        .filter(|f| matches!(f.status, Status::Warn | Status::Blocked))
        .count();

    if applied.is_empty() {
        if degraded == 0 {
            println!("airc doctor: ok ({} check(s) clean)", findings.len());
        } else {
            println!(
                "airc doctor: {degraded} of {} check(s) need attention. Re-run with --fix to apply safe auto-recovery.",
                findings.len()
            );
        }
    } else {
        println!("airc doctor: applied {} fix(es):", applied.len());
        for action in &applied {
            println!("  • {action}");
        }
        println!("Re-run `airc doctor` to verify.");
    }

    Ok(())
}

async fn apply_fixes(
    home: &Path,
    findings: &[Finding],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut applied = Vec::new();
    for finding in findings {
        if finding.check == "daemon" && finding.status == Status::Warn {
            // Stale socket case. Identity PartialState recovery is
            // intentionally NOT automatic — wiping a peer_id
            // discards trust enrolled by remote peers; surface the
            // manual one-liner instead.
            let socket = crate::cli::default_socket_path_in(home);
            if socket.exists() {
                match std::fs::remove_file(&socket) {
                    Ok(()) => {
                        applied.push(format!(
                            "removed stale daemon socket at {}",
                            socket.display()
                        ));
                    }
                    Err(error) => {
                        eprintln!(
                            "doctor: couldn't remove stale socket {}: {error}",
                            socket.display()
                        );
                    }
                }
            }
        }
    }
    Ok(applied)
}

// Re-export the run signature behind a simpler module path used by
// the dispatch site in main.rs.
pub async fn run_doctor(
    home: &Path,
    fix: bool,
    health: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    run(home, fix, health).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches: a registration mistake in [`registry`] — a
    /// copy-pasted `impl` that kept the donor's name, or a check added twice.
    /// Both render as duplicate/ambiguous lines in the report and are
    /// invisible in review, because the walk itself no longer names anyone.
    /// Also pins that BOTH tiers stay populated: if every check drifted to
    /// `Health`, plain `airc doctor` would silently report nothing at all.
    #[test]
    fn registry_names_are_unique_and_both_tiers_are_populated() {
        let configs: Vec<CheckConfig> = registry().iter().map(|c| c.config()).collect();
        let mut names: Vec<&str> = configs.iter().map(|c| c.name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate check name in registry");
        assert!(names.iter().all(|n| !n.is_empty()), "empty check name");
        assert!(
            configs.iter().any(|c| c.tier == CheckTier::Always),
            "plain `airc doctor` must run at least one check"
        );
        assert!(
            configs.iter().any(|c| c.tier == CheckTier::Health),
            "`--health` must add at least one check, or the flag is a lie"
        );
    }

    #[test]
    fn status_labels_match_skill_doc() {
        // The skill markdown uses [ok] [info] [WARN] [BLOCKED] —
        // pin those literally so future-doctor renderings don't
        // drift from operator-readable docs.
        assert_eq!(Status::Ok.label(), "[ok]");
        assert_eq!(Status::Info.label(), "[info]");
        assert_eq!(Status::Warn.label(), "[WARN]");
        assert_eq!(Status::Blocked.label(), "[BLOCKED]");
    }

    #[test]
    fn short_sha_truncates_and_preserves_short_input() {
        // what this catches: the 12-char display contract matches
        // `airc status`'s `build:` rendering, and short inputs aren't
        // over-sliced (no panic on a sub-12-char sha).
        assert_eq!(short_sha("abcdef1234567890abcdef"), "abcdef123456");
        assert_eq!(short_sha("abc123"), "abc123");
    }
}
