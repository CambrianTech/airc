//! `airc doctor` — install + identity + scope self-diagnosis with
//! optional auto-recovery.
//!
//! The skill documents agents calling this; the binary owns the
//! diagnostic walk. Each check returns a [`Finding`] with a status
//! (`Ok`, `Info`, `Warn`, `Blocked`) plus the exact one-liner fix
//! the operator (or agent) should run. With `--fix`, doctor applies
//! the safe auto-recoveries inline.
//!
//! Diagnostic surface (in priority order):
//!
//! 1. **Identity** — `identity.key` + `local_identity` row pairing.
//!    Detects partial state (most common new-machine friction).
//!    Identity repair is intentionally manual because wiping a
//!    peer_id discards remote trust enrolled against that id.
//!
//! 2. **Daemon liveness** — is a daemon process answering the IPC
//!    socket for this scope? Stale socket vs missing entirely.
//!    Auto-fix on `--fix`: remove a stale socket file.
//!
//! 3. **Binary freshness** — does the installed binary match the
//!    source tree, if a source tree is detectable? Surfaces "old
//!    binary on PATH" — the symptom I (claude) hit when running
//!    pre-#885 binary against post-#885 schema.
//!
//! 4. **Route + transport health** (with `--health`) — calls into
//!    `Airc::refresh_route_discovery` for the typed transport
//!    health snapshot and renders it.
//!
//! Each Finding maps to either a single-line stdout report
//! (default mode) or an action (fix mode). The skill markdown can
//! be the AI-side narration layer over this binary.

use std::path::Path;

use airc_identity::LocalIdentity;
use airc_ipc::DaemonClient;

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

pub async fn run(home: &Path, fix: bool, health: bool) -> Result<(), Box<dyn std::error::Error>> {
    println!("airc doctor — scope: {}", home.display());
    println!();

    let mut applied = Vec::new();
    let mut findings = Vec::new();

    findings.extend(check_identity(home).await);
    findings.extend(check_daemon(home).await);
    findings.extend(check_binary_freshness());
    findings.extend(check_recent_diagnostics(home).await);

    if health {
        findings.extend(check_health(home).await);
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

/// Identity check — the most common new-machine friction. Walks the
/// same partial-state logic `LocalIdentity::load_or_generate` does
/// but reports rather than fails.
async fn check_identity(home: &Path) -> Vec<Finding> {
    let key_path = LocalIdentity::key_path(home);
    let key_exists = key_path.exists();
    // Probe legacy json so a half-migrated install is named for what
    // it is, not just "row missing".
    let legacy_json = home.join("identity.json").exists();

    // Open the store to ask about the singleton row. If the store
    // itself can't open, surface that instead — that's a different
    // class of breakage (disk full, permissions, db corruption).
    let store = match airc_store::SqliteEventStore::open_path(&home.join("events.sqlite")).await {
        Ok(store) => store,
        Err(error) => {
            return vec![Finding::blocked(
                "identity store",
                format!("can't open events.sqlite: {error}"),
                "check disk/permissions; if corrupted, `airc stop` then `rm <home>/events.sqlite` and `airc join` to rebuild (loses scope state)",
            )];
        }
    };
    let row = match store.load_local_identity().await {
        Ok(opt) => opt,
        Err(error) => {
            return vec![Finding::blocked(
                "identity row",
                format!("can't query local_identity: {error}"),
                "schema may be from an older binary; `airc update` or rebuild",
            )];
        }
    };

    match (key_exists, row.is_some(), legacy_json) {
        (true, true, _) => vec![Finding::ok(
            "identity",
            "key + ORM row both present",
        )],
        (false, false, false) => vec![Finding::info(
            "identity",
            "no identity material (fresh scope; `airc join` will generate)",
        )],
        (false, false, true) => vec![Finding::warn(
            "identity",
            "legacy identity.json present without identity.key — orphan metadata",
            "`rm <home>/identity.json` then `airc join` to regenerate identity cleanly",
        )],
        (true, false, true) => vec![Finding::warn(
            "identity",
            "key present + legacy identity.json present, no ORM row — pre-#902 install",
            "`airc join` will auto-migrate (post-#902 logic; identity.json gets consumed)",
        )],
        (true, false, false) => vec![Finding::blocked(
            "identity",
            "key present but no ORM row and no legacy json — orphan key, no recovery without backup",
            "`airc stop` then `rm <home>/identity.key` (loses peer_id), then `airc join` to regenerate",
        )],
        (false, true, _) => vec![Finding::blocked(
            "identity",
            "ORM row present but key file missing — can't sign without the secret",
            "restore <home>/identity.key from backup, OR `airc stop` + `rm -rf <home>` then `airc join` (loses peer_id)",
        )],
    }
}

async fn check_daemon(home: &Path) -> Vec<Finding> {
    let socket = crate::cli::default_socket_path_in(home);
    let client = DaemonClient::new(socket.clone());
    match client
        .ping_with_timeout(std::time::Duration::from_millis(250))
        .await
    {
        Ok(_) => vec![Finding::ok(
            "daemon",
            format!("responding on {}", socket.display()),
        )],
        Err(_) if socket.exists() => vec![Finding::warn(
            "daemon",
            format!(
                "socket exists at {} but no process answers",
                socket.display()
            ),
            format!(
                "stale socket from prior crash; remove with `rm {}` then `airc join`",
                socket.display()
            ),
        )],
        Err(_) => vec![Finding::info(
            "daemon",
            "not running (`airc join` will spawn it)",
        )],
    }
}

fn check_binary_freshness() -> Vec<Finding> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return vec![Finding::info("binary", "couldn't resolve current exe path")],
    };
    let canonical = exe.canonicalize().unwrap_or_else(|_| exe.clone());

    let mut findings = vec![Finding::info(
        "binary",
        format!("install: {}", canonical.display()),
    )];

    // Compare the baked-in build sha (from build.rs) against the
    // current HEAD of the install source tree. If they diverge, the
    // installed binary is stale relative to its source checkout —
    // running `airc update` reconciles it.
    if !crate::build_info::is_unknown() {
        findings.push(Finding::info(
            "binary",
            format!(
                "build: {} on {}",
                crate::build_info::COMMIT_SHORT,
                crate::build_info::BRANCH
            ),
        ));
        if let Some(source_head) = source_tree_head() {
            if source_head == crate::build_info::COMMIT {
                findings.push(Finding::ok(
                    "binary",
                    "installed binary matches source checkout HEAD",
                ));
            } else {
                let short_source = &source_head[..source_head.len().min(12)];
                findings.push(Finding::warn(
                    "binary",
                    format!(
                        "installed binary drifted from source tree (binary={} source={short_source})",
                        crate::build_info::COMMIT_SHORT
                    ),
                    "run `airc update` to reconcile",
                ));
            }
        }
    } else {
        findings.push(Finding::info(
            "binary",
            "build sha unknown (git unavailable at compile time); skipping drift check",
        ));
    }

    findings
}

fn source_tree_head() -> Option<String> {
    // The install source path is conventionally `~/.airc/src` per
    // install.sh, but we resolve it the same way `update_commands`
    // does so the two stay aligned.
    let source = crate::update_commands::install_source_dir().ok()?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&source)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Recent diagnostic visibility check. Pulls the last N transcript
/// events, decodes typed `DiagnosticEvent`s emitted via the AIRC
/// event sink, and surfaces error/warn counts so operators see
/// substrate trouble without inspecting the wire by hand.
async fn check_recent_diagnostics(home: &Path) -> Vec<Finding> {
    use airc_diagnostics::DiagnosticSeverity;
    use airc_lib::Airc;

    let airc = match Airc::open(home).await {
        Ok(airc) => airc,
        Err(_) => {
            return vec![Finding::info(
                "diagnostics",
                "airc handle unavailable; skipping recent-diagnostic scan",
            )];
        }
    };

    let recent = match airc.recent_diagnostic_events(256).await {
        Ok(recent) => recent,
        Err(_) => {
            return vec![Finding::info(
                "diagnostics",
                "couldn't read recent diagnostics from transcript",
            )];
        }
    };

    if recent.is_empty() {
        return vec![Finding::ok(
            "diagnostics",
            "no recent diagnostic events on the wire",
        )];
    }

    let mut errors = 0usize;
    let mut warns = 0usize;
    for diag in &recent {
        match diag.severity {
            DiagnosticSeverity::Error => errors += 1,
            DiagnosticSeverity::Warn => warns += 1,
            DiagnosticSeverity::Info | DiagnosticSeverity::Debug => {}
        }
    }

    if errors > 0 {
        vec![Finding::warn(
            "diagnostics",
            format!(
                "{errors} error / {warns} warn diagnostic(s) in last {} events",
                recent.len()
            ),
            "review with `airc events list --header-prefix airc.diag.severity=`",
        )]
    } else if warns > 0 {
        vec![Finding::info(
            "diagnostics",
            format!(
                "{warns} warn diagnostic(s) in last {} events; no errors",
                recent.len()
            ),
        )]
    } else {
        vec![Finding::ok(
            "diagnostics",
            format!("{} diagnostic event(s); none at warn/error", recent.len()),
        )]
    }
}

async fn check_health(home: &Path) -> Vec<Finding> {
    use airc_lib::{Airc, TransportHealthState};

    let airc = match Airc::open(home).await {
        Ok(airc) => airc,
        Err(error) => {
            return vec![Finding::blocked(
                "route health",
                format!("can't open substrate: {error}"),
                "address the identity/store errors above first",
            )];
        }
    };
    let snapshot = match airc.refresh_route_discovery().await {
        Ok(s) => s,
        Err(error) => {
            return vec![Finding::warn(
                "route health",
                format!("route refresh failed: {error}"),
                "run `airc transport health` for the underlying detail",
            )];
        }
    };
    let total = snapshot.health.len();
    let degraded = snapshot
        .health
        .iter()
        .filter(|sample| sample.state != TransportHealthState::Healthy)
        .count();
    if degraded == 0 {
        vec![Finding::ok(
            "route health",
            format!("{total} route(s) healthy"),
        )]
    } else {
        vec![Finding::warn(
            "route health",
            format!("{degraded} of {total} route(s) degraded"),
            "run `airc transport health` to see the row-level detail",
        )]
    }
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

    #[tokio::test]
    async fn fresh_scope_reports_no_identity_material() {
        let dir = tempfile::TempDir::new().unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Info);
        assert!(findings[0].detail.contains("no identity material"));
    }

    #[tokio::test]
    async fn key_without_row_is_blocked_with_clear_fix() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("identity.key"), [7u8; 32]).unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Blocked);
        let fix = findings[0].fix.as_ref().unwrap();
        assert!(fix.contains("teardown --flush"));
    }

    #[tokio::test]
    async fn key_plus_legacy_json_reports_pre_902_install() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("identity.key"), [7u8; 32]).unwrap();
        std::fs::write(dir.path().join("identity.json"), "{}").unwrap();
        let findings = check_identity(dir.path()).await;
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].status, Status::Warn);
        assert!(findings[0].detail.contains("pre-#902"));
    }
}
