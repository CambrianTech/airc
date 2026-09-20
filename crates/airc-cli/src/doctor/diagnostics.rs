//! Recent-diagnostic visibility — surfaces typed `DiagnosticEvent`s
//! emitted via the AIRC event sink so operators see substrate trouble
//! without inspecting the wire by hand.
//!
//! The subtlety is entirely in the EMPTY case: see [`render_diagnostic_scan`].

use std::path::Path;

use super::{Check, CheckConfig, CheckContext, Finding};

/// Recent diagnostics on the wire. Opens the substrate handle but reads only
/// the local transcript — no dialing — so it stays in the always-on tier.
pub(super) struct DiagnosticsCheck;

#[async_trait::async_trait]
impl Check for DiagnosticsCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::always("diagnostics")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_recent_diagnostics(ctx.home).await
    }
}

async fn check_recent_diagnostics(home: &Path) -> Vec<Finding> {
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

    const DIAG_SCAN_WINDOW: usize = 256;
    let scan = match airc.recent_diagnostics(DIAG_SCAN_WINDOW).await {
        Ok(scan) => scan,
        Err(_) => {
            return vec![Finding::info(
                "diagnostics",
                "couldn't read recent diagnostics from transcript",
            )];
        }
    };

    vec![render_diagnostic_scan(&scan)]
}

/// Turn a diagnostic scan into a verdict. Pure: no IO, fully testable.
///
/// The empty case is the load-bearing one. Diagnostics are published as
/// `FrameKind::Event`, which maps to `TranscriptKind::System` — the SAME
/// transcript kind as agent heartbeats, stream chunks and lane-coordination
/// frames — so the scan cannot filter them out by kind and is a raw newest-N
/// page. On a node carrying real traffic those N events are heartbeats, every
/// diagnostic has been evicted, and the check used to answer `[ok] no recent
/// diagnostic events on the wire`. That is an inverted absence gate: it goes
/// blind exactly when the node is busy, which is exactly when you are reading
/// it. Absence is only a clean bill of health when the window was NOT full.
fn render_diagnostic_scan(scan: &airc_lib::RecentDiagnostics) -> Finding {
    use airc_diagnostics::DiagnosticSeverity;

    if scan.events.is_empty() {
        return if scan.establishes_clean() {
            Finding::ok(
                "diagnostics",
                format!(
                    "no diagnostic events on the wire (scanned all {} event(s) in this room)",
                    scan.scanned
                ),
            )
        } else {
            Finding::info(
                "diagnostics",
                format!(
                    "no diagnostics in the newest {} event(s), but the scan window was FULL of \
                     other traffic — older diagnostics lie beyond it, so this is 'not visible \
                     from here', not 'clean'",
                    scan.scanned
                ),
            )
        };
    }

    let mut errors = 0usize;
    let mut warns = 0usize;
    for diag in &scan.events {
        match diag.severity {
            DiagnosticSeverity::Error => errors += 1,
            DiagnosticSeverity::Warn => warns += 1,
            DiagnosticSeverity::Info | DiagnosticSeverity::Debug => {}
        }
    }
    let found = scan.events.len();

    if errors > 0 {
        Finding::warn(
            "diagnostics",
            format!("{errors} error / {warns} warn diagnostic(s) in last {found} events"),
            "review with `airc events list --header-prefix airc.diag.severity=`",
        )
    } else if warns > 0 {
        Finding::info(
            "diagnostics",
            format!("{warns} warn diagnostic(s) in last {found} events; no errors"),
        )
    } else {
        Finding::ok(
            "diagnostics",
            format!("{found} diagnostic event(s); none at warn/error"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Status;

    fn scan(
        events: Vec<airc_diagnostics::DiagnosticEvent>,
        scanned: usize,
        full: bool,
    ) -> airc_lib::RecentDiagnostics {
        airc_lib::RecentDiagnostics {
            events,
            scanned,
            window_saturated: full,
        }
    }

    fn diag(severity: airc_diagnostics::DiagnosticSeverity) -> airc_diagnostics::DiagnosticEvent {
        use airc_diagnostics::{DiagnosticCode, DiagnosticComponent, DiagnosticEvent};
        DiagnosticEvent {
            severity,
            component: DiagnosticComponent::Transport,
            code: DiagnosticCode::ConnectionError,
            message: "t".into(),
            fields: Default::default(),
            occurred_at_ms: 0,
        }
    }

    /// what this catches: an INVERTED absence gate — the diagnostic check went
    /// blind exactly when the node was busy. Diagnostics share
    /// TranscriptKind::System with heartbeats and stream chunks, so the scan is
    /// a raw newest-256 page; on a node carrying traffic every diagnostic is
    /// evicted and the old code answered `[ok] no recent diagnostic events on
    /// the wire` while errors were being emitted. Measured 2026-08-05 on
    /// BIGMAMA: `airc events list --limit 80` returned 79 heartbeats and 1
    /// message. A FULL window can never be reported as clean.
    #[test]
    fn a_full_scan_window_is_not_visible_from_here_never_clean() {
        let full = render_diagnostic_scan(&scan(vec![], 256, true));
        assert_eq!(
            full.status,
            Status::Info,
            "a saturated window must not claim a clean diagnostic surface"
        );
        assert!(full.detail.contains("not visible"), "{}", full.detail);

        // The genuinely-quiet node still gets its [ok] — the fix must not
        // turn every healthy scope into a permanent info line.
        let quiet = render_diagnostic_scan(&scan(vec![], 12, false));
        assert_eq!(quiet.status, Status::Ok);
        assert!(quiet.detail.contains("scanned all 12"), "{}", quiet.detail);
    }

    #[test]
    fn diagnostics_found_still_rank_by_severity() {
        // what this catches: the empty-case rework must leave the case the
        // check exists for intact — errors warn, warns inform, clean is ok.
        use airc_diagnostics::DiagnosticSeverity::{Error, Info as DInfo, Warn};
        assert_eq!(
            render_diagnostic_scan(&scan(vec![diag(Error), diag(Warn)], 256, true)).status,
            Status::Warn
        );
        assert_eq!(
            render_diagnostic_scan(&scan(vec![diag(Warn)], 256, true)).status,
            Status::Info
        );
        assert_eq!(
            render_diagnostic_scan(&scan(vec![diag(DInfo)], 256, true)).status,
            Status::Ok
        );
    }

    #[test]
    fn establishes_clean_requires_both_empty_and_unsaturated() {
        // what this catches: the whole disambiguation collapses if this
        // predicate ever ignores saturation.
        assert!(scan(vec![], 12, false).establishes_clean());
        assert!(!scan(vec![], 256, true).establishes_clean());
        assert!(!scan(
            vec![diag(airc_diagnostics::DiagnosticSeverity::Warn)],
            12,
            false
        )
        .establishes_clean());
    }
}
