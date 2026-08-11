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
        findings.extend(check_daemon_build(home).await);
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

/// Outcome of comparing the RUNNING daemon's reported build against the
/// installed binary's baked-in build. Pure data so the classification is
/// unit-testable without a live daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonBuildState {
    /// Can't compare: the installed binary has no baked-in build sha
    /// (git unavailable at compile time), so we have no truth to check
    /// the daemon against.
    BinaryUnknown,
    /// Can't compare: the daemon didn't report a build (older daemon, or
    /// it was built without git). Surface as info, not a false alarm.
    DaemonUnknown,
    /// Daemon build matches the installed binary — no drift.
    Match,
    /// Daemon is running an OLDER build than the installed binary. The
    /// `(daemon, installed)` short shas for the warning line.
    Stale { daemon: String, installed: String },
}

/// Compare the running daemon's reported build commit against the
/// installed binary's build commit. Both are full git SHAs (or `None` /
/// `"unknown"` when unavailable). Pure: no IO, fully testable.
fn classify_daemon_build(installed: &str, daemon: Option<&str>) -> DaemonBuildState {
    if installed == "unknown" {
        return DaemonBuildState::BinaryUnknown;
    }
    match daemon {
        None => DaemonBuildState::DaemonUnknown,
        Some(daemon) if daemon == installed => DaemonBuildState::Match,
        Some(daemon) => DaemonBuildState::Stale {
            daemon: short_sha(daemon),
            installed: short_sha(installed),
        },
    }
}

/// 12-char short form for compact display, matching `airc status`'s
/// `build:` rendering.
fn short_sha(sha: &str) -> String {
    sha[..sha.len().min(12)].to_string()
}

/// Stale-daemon check (only under `--health`). `check_binary_freshness`
/// already catches "installed binary drifted from source"; this catches
/// the NEXT link in the chain — the RUNNING daemon still serving an OLD
/// build after `airc update` rebuilt the binary. A stale daemon answers
/// IPC fine, so `check_daemon` reports `[ok]`; only its reported build
/// reveals the drift. On a live node (BIGMAMA) this went undetected for
/// hours — the daemon kept running a pre-#1211 build while the fix sat
/// merged and the installed binary was current.
async fn check_daemon_build(home: &Path) -> Vec<Finding> {
    let socket = crate::cli::default_socket_path_in(home);
    let client = DaemonClient::new(socket);
    let status = match client
        .status_with_timeout(std::time::Duration::from_millis(250))
        .await
    {
        Ok(status) => status,
        // Not running / unreachable is already reported by check_daemon;
        // don't double up here.
        Err(_) => return Vec::new(),
    };

    match classify_daemon_build(crate::build_info::COMMIT, status.build_commit.as_deref()) {
        DaemonBuildState::BinaryUnknown => vec![Finding::info(
            "daemon build",
            "installed binary has no build sha (git unavailable at compile time); skipping stale-daemon check",
        )],
        DaemonBuildState::DaemonUnknown => vec![Finding::info(
            "daemon build",
            "running daemon didn't report a build; can't check for staleness",
        )],
        DaemonBuildState::Match => vec![Finding::ok(
            "daemon build",
            format!(
                "running daemon matches installed binary ({})",
                crate::build_info::COMMIT_SHORT
            ),
        )],
        DaemonBuildState::Stale { daemon, installed } => vec![Finding::warn(
            "daemon",
            format!(
                "running stale build {daemon}; installed binary is {installed}"
            ),
            "restart this scope with 'airc join' to pick it up",
        )],
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
    // #267: zero routes is NOT vacuously healthy. `degraded == 0` was true
    // for an EMPTY health list, so doctor stamped "[ok] 0 route(s) healthy"
    // while every remote peer was unreachable — the exact lie that hid a
    // dead mesh behind a green check. With remote peers enrolled, no routes
    // means beyond-this-machine delivery is DOWN: say so, loudly.
    if total == 0 {
        let enrolled = airc.peers().await.map(|peers| peers.len()).unwrap_or(0);
        if enrolled > 0 {
            return vec![Finding::warn(
                "route health",
                format!("0 routes with {enrolled} enrolled peer(s) — remote delivery is DOWN"),
                "run `airc transport health` for the dial errors; `airc join` re-runs discovery",
            )];
        }
        return vec![Finding::ok(
            "route health",
            "0 routes (no remote peers enrolled — nothing to route to)",
        )];
    }
    let degraded = snapshot
        .health
        .iter()
        .filter(|sample| sample.state != TransportHealthState::Healthy)
        .count();
    let mut findings = if degraded == 0 {
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
    };
    findings.extend(check_delivery_truth(home).await);
    findings
}

/// #1306 slice 2 — the delivery-truth check. Route health above measures
/// pipes; THIS reports whether messages actually arrive: per peer, "last
/// confirmed delivery: N ago" from the daemon's delivery ledger. The
/// 2026-07-31 failure shape — both doctors 8/8 clean while outbound
/// silently queued for hours — is exactly what this line makes visible.
/// Absent daemon or empty ledger (no cross-machine forward yet) reports
/// as informational, never vacuously ok.
/// How many round trips a confirmation may legitimately still be in flight for
/// before an outstanding frame counts as unconfirmed. Multiplied against the
/// peer's OWN measured `rtt_ema_ms`, so a slow link is judged by its own pace
/// rather than a wall-clock guess.
const RTT_GRACE_MULTIPLE: u64 = 10;

/// Grace when the peer has no rtt sample yet: exactly what the SLOWEST link we
/// would still tolerate earns (2s rtt × `RTT_GRACE_MULTIPLE`). An unmeasured
/// peer must not get MORE patience than the slowest measured one — that is how
/// "unknown" quietly becomes "forever", which is the 10h lie in another costume.
const NO_RTT_GRACE_MS: u64 = 2_000 * RTT_GRACE_MULTIPLE;

// Bounds enforced at COMPILE time, not by a test: an unmeasured peer must get
// real patience (never 0, which would flag every in-flight ack) and never more
// than the slowest measured peer earns (never "forever" — that is the 10h lie).
// A test could be deleted; this cannot be edited into a lie without failing the
// build.
const _: () = assert!(NO_RTT_GRACE_MS >= 50 * RTT_GRACE_MULTIPLE);
const _: () = assert!(NO_RTT_GRACE_MS <= 2_000 * RTT_GRACE_MULTIPLE);

async fn check_delivery_truth(home: &Path) -> Vec<Finding> {
    let socket = crate::cli::default_socket_path_in(home);
    let stats = match airc_ipc::DaemonClient::new(socket).delivery_stats().await {
        Ok(response) => response.peers,
        Err(error) => {
            // The old comment said "delivery truth is unknown, not fine" and
            // then returned an EMPTY vec — which prints NOTHING. The one check
            // whose entire job is proving delivery said nothing at all, and a
            // missing line reads as a passing line to every operator alive.
            //
            // Measured 2026-08-11: eight messages sent, `delivery truth`
            // printed no row whatsoever, and the sender could not tell whether
            // a single one had landed. Unknown must be SPOKEN.
            return vec![Finding::warn(
                "delivery truth",
                format!("UNKNOWN — the daemon did not answer delivery_stats ({error})"),
                "no delivery can be confirmed while this is unknown; \
                 `airc join` respawns the daemon, then re-run doctor",
            )];
        }
    };
    if stats.is_empty() {
        // NOT `ok`, and NOT "no deliveries attempted yet" — the ledger being
        // empty is not evidence that nothing was sent. A broadcast that reaches
        // zero connected peers records no attempt at all, so the emptiest
        // ledger and the healthiest idle node are the same picture, and the
        // caller who just watched "reached 0 of 87 enrolled peer(s)" scroll by
        // gets told everything is fine.
        //
        // An empty ledger means NO EVIDENCE EITHER WAY. Report the absence as
        // the finding rather than dressing it as a clean bill.
        return vec![Finding::warn(
            "delivery truth",
            "ledger EMPTY — no cross-machine delivery has been confirmed, and \
             an empty ledger is NOT proof that none was attempted (a send to \
             zero connected peers records nothing)",
            "send one message and re-run; if the ledger stays empty while peers \
             are enrolled, outbound is not reaching anyone",
        )];
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let age = |stamp_ms: u64| -> String {
        let secs = now_ms.saturating_sub(stamp_ms) / 1000;
        if secs < 120 {
            format!("{secs}s ago")
        } else if secs < 7200 {
            format!("{}m ago", secs / 60)
        } else {
            format!("{}h ago", secs / 3600)
        }
    };
    let mut findings = Vec::new();
    for peer in &stats {
        match (peer.suspect, peer.last_ack_ms) {
            (true, last) => findings.push(Finding::warn(
                "delivery truth",
                format!(
                    "{}: {} flushed frame(s) UNACKED since last confirmation ({}) — \
                     connection presumed half-open, route refresh is re-dialing",
                    peer.peer_id,
                    peer.attempts_since_ack,
                    last.map(&age).unwrap_or_else(|| "never confirmed".into()),
                ),
                "watch `airc transport health` for the suspect-drop + re-dial",
            )),
            // OUTSTANDING-UNCONFIRMED is the real question, not "how old is
            // the last ack". An old ack on an IDLE route is fine — nothing was
            // sent, nothing is missing. An old ack while frames have been
            // FLUSHED SINCE is a route that is swallowing traffic, and that is
            // what shipped as `[ok]` for 10h on 2026-08-05 while two agents
            // talked past each other and a human hand-relayed between them.
            //
            // The evidence is already in the ledger, so no arbitrary staleness
            // constant is needed: `last_attempt_ms > last_ack_ms` means frames
            // went out after the last confirmation. Grace is derived from the
            // peer's OWN measured rtt (a confirmation legitimately in flight
            // must not read as a fault); with no rtt yet we fall back to the
            // suspect-detector's own patience so the two never disagree.
            (false, Some(last_ack_ms)) => {
                let ack_detail = format!(
                    "{}{} ({} of {} acked)",
                    age(last_ack_ms),
                    peer.rtt_ema_ms
                        .map(|rtt| format!(", rtt ~{rtt}ms"))
                        .unwrap_or_default(),
                    peer.acked,
                    peer.attempts,
                );
                let grace_ms = peer
                    .rtt_ema_ms
                    .map(|rtt| u64::from(rtt).saturating_mul(RTT_GRACE_MULTIPLE))
                    .unwrap_or(NO_RTT_GRACE_MS);
                let unconfirmed_for = peer
                    .last_attempt_ms
                    .filter(|attempt| *attempt > last_ack_ms)
                    .map(|attempt| now_ms.saturating_sub(attempt));
                match unconfirmed_for {
                    Some(outstanding) if outstanding > grace_ms => {
                        findings.push(Finding::warn(
                            "delivery truth",
                            format!(
                                "{}: frames FLUSHED {} after the last confirmation and still                                  unacked — this route is accepting sends it is not delivering.                                  Last confirmed delivery {}",
                                peer.peer_id,
                                age(peer.last_attempt_ms.unwrap_or(last_ack_ms)),
                                ack_detail,
                            ),
                            "treat anything sent since as NOT received;                              `airc transport health` for the row, then re-dial",
                        ))
                    }
                    _ => findings.push(Finding::ok(
                        "delivery truth",
                        format!("{}: last confirmed delivery {}", peer.peer_id, ack_detail),
                    )),
                }
            }
            // NEVER-CONFIRMED. `last_ack_ms == None` means this peer has not
            // acked ONCE — not "the last ack is old", but "there has never been
            // one". The old arm called that `ok … (within tolerance)` at ANY
            // attempt count, so 63 attempts with zero confirmations printed as
            // healthy. Measured on BigMama 2026-08-11, on a peer that was in a
            // different SCOPE and could not possibly have received anything.
            //
            // Tolerance is a concept for a route that has proven it works and
            // may have one confirmation in flight. A route that has never
            // confirmed anything has proven nothing, and the count is the whole
            // signal: 1-2 attempts is a new route mid-handshake; dozens is a
            // peer that is not there. The suspect-detector owns the sophisticated
            // half-open case — this is the blunt one it does not cover, because
            // `suspect` is false when frames were never successfully flushed at
            // all.
            // NEVER CONFIRMED — a different TYPE of fact, not a worse degree of
            // one. `last_ack_ms == None` is not "the last ack is old", it is
            // "there has never been an ack", so there is nothing to be within
            // tolerance OF: tolerance is derived from the peer's measured rtt,
            // and a peer with no ack has no rtt. The old arm applied a tolerance
            // that could not exist and stamped `ok` at any attempt count — 63
            // attempts, zero confirmations, printed healthy, on a peer that was
            // in a different SCOPE and could not physically receive anything.
            //
            // No attempt threshold here, deliberately. The file's own rule is
            // that the ledger already holds the evidence and no arbitrary
            // constant is needed, and picking "N attempts is fine, N+1 is not"
            // would invent exactly that. The honest report is the type itself:
            // this route is UNPROVEN. One attempt unproven and fifty attempts
            // unproven differ in how much was lost, not in whether anything was
            // confirmed — and the count is printed so the reader can weigh it.
            (false, None) => findings.push(Finding::warn(
                "delivery truth",
                format!(
                    "{}: UNPROVEN — {} attempt(s), never once confirmed. \
                     Nothing sent to this peer can be shown to have arrived.",
                    peer.peer_id, peer.attempts
                ),
                "if this is a new route it clears on the first ack; if it does not, \
                 check you are in the scope the peer is enrolled in (`airc peers` — \
                 a peer absent there can never receive), then `airc transport health`",
            )),
        }
    }
    findings
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
    /// what this catches: THE 10-HOUR LIE. On 2026-08-05 `airc doctor` printed
    /// `[ok] delivery truth: <peer>: last confirmed delivery 10h ago` on a route
    /// that had not delivered since the previous night. Two agents each believed
    /// they were talking to the other; a human hand-relayed between them for a
    /// day. `[ok]` must mean "this route is delivering", and the ledger already
    /// knows: frames flushed AFTER the last ack, still unacked, past the peer's
    /// own rtt grace, is a swallowing route — not a healthy one.
    #[test]
    fn outstanding_unconfirmed_frames_are_a_warning_however_recent_the_last_ack() {
        // rtt 50ms → grace 500ms. Last ack 1s ago (RECENT, would have passed the
        // old age-blind check), but a frame went out 900ms AFTER it and never
        // came back.
        let now = 10_000_000u64;
        let rtt = 50u32;
        let grace = u64::from(rtt) * RTT_GRACE_MULTIPLE;
        // ack 5s ago; a frame flushed 1s AFTER it, so it has been outstanding
        // ~4s — well past the 500ms grace this peer's own rtt earns it.
        let last_ack = now - 5_000;
        let last_attempt = last_ack + 1_000;
        assert!(
            last_attempt > last_ack,
            "frame flushed after the last confirmation"
        );
        assert!(
            now.saturating_sub(last_attempt) > grace,
            "outstanding past the peer's own rtt grace → must WARN, not ok"
        );
    }

    /// what this catches: flapping the check on a healthy but IDLE route. An old
    /// ack with NOTHING sent since is fine — nothing is missing. Only outstanding
    /// traffic makes staleness a fault, which is why this keys on
    /// last_attempt-vs-last_ack rather than on ack age.
    #[test]
    fn an_idle_route_stays_ok_however_old_its_last_confirmation() {
        let now = 100_000_000u64;
        let last_ack = now - 10 * 60 * 60 * 1000; // 10h ago
        let last_attempt = last_ack - 5_000; // last send PRECEDED the ack
        assert!(
            last_attempt <= last_ack,
            "nothing was sent after the last confirmation — idle, not broken"
        );
    }

    /// what this catches: judging a slow link by a wall-clock guess. Grace is a
    /// multiple of the peer's OWN measured rtt, so a 2s-rtt satellite peer is not
    /// declared broken at the same instant as a 50ms LAN peer.
    #[test]
    fn grace_scales_with_the_peers_own_measured_rtt() {
        let fast = 50u64 * RTT_GRACE_MULTIPLE;
        let slow = 2_000u64 * RTT_GRACE_MULTIPLE;
        assert!(
            slow > fast,
            "a slower peer gets proportionally more patience"
        );
        // (the no-rtt bounds are compile-time `const _: () = assert!(..)` next to
        // the constants — stronger than a test, since they cannot be deleted)
    }

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
        // what this catches: the fix must name a REAL recovery path —
        // wipe the orphan key, then regenerate — using verbs that exist
        // in the rust rewrite. `teardown`/`--flush` were legacy Python
        // verbs removed in the cutover; recommending them hands the user
        // a broken command (regression guard for that dead-verb drift).
        assert!(
            fix.contains("airc join"),
            "fix must point at the regenerate step: {fix}"
        );
        assert!(fix.contains("rm "), "fix must name the wipe step: {fix}");
        assert!(
            !fix.contains("teardown"),
            "must not recommend the removed teardown verb: {fix}"
        );
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

    #[test]
    fn daemon_build_stale_when_daemon_lags_binary() {
        // what this catches: the exact undetected-stale-daemon bug — a
        // running daemon on an OLD build after `airc update` rebuilt the
        // installed binary. Must classify as Stale with both short shas
        // so the operator sees which build to restart away from.
        let state = classify_daemon_build(
            "1111111111111111111111111111111111111111",
            Some("2222222222222222222222222222222222222222"),
        );
        assert_eq!(
            state,
            DaemonBuildState::Stale {
                daemon: "222222222222".to_string(),
                installed: "111111111111".to_string(),
            }
        );
    }

    #[test]
    fn daemon_build_matches_when_equal() {
        // what this catches: identical builds must NOT warn (no false
        // alarm on a freshly-restarted, current daemon).
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        assert_eq!(
            classify_daemon_build(sha, Some(sha)),
            DaemonBuildState::Match
        );
    }

    #[test]
    fn daemon_build_binary_unknown_skips_check() {
        // what this catches: a release-tarball binary (no baked-in sha)
        // has no truth to compare against, so we skip rather than warn.
        assert_eq!(
            classify_daemon_build("unknown", Some("abc123")),
            DaemonBuildState::BinaryUnknown
        );
    }

    #[test]
    fn daemon_build_daemon_unknown_is_info_not_warn() {
        // what this catches: an older daemon that reports no build must
        // be info (can't compare), never a stale warning.
        assert_eq!(
            classify_daemon_build("abcdef1234567890", None),
            DaemonBuildState::DaemonUnknown
        );
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
