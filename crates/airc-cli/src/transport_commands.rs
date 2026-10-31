//! Transport health consumes the daemon observations interpreted by doctor.
//! A short-lived CLI handle must never manufacture a healthy local route view.
use std::path::Path;

use crate::doctor::{Finding, Status};
use serde::Serialize;

#[derive(Serialize)]
struct HealthReport {
    requires_attention: bool,
    findings: Vec<Finding>,
}

impl HealthReport {
    fn new(findings: Vec<Finding>) -> Self {
        let requires_attention = findings.is_empty()
            || findings
                .iter()
                .any(|finding| needs_attention(finding.status));
        Self {
            requires_attention,
            findings,
        }
    }
}

fn needs_attention(status: Status) -> bool {
    matches!(status, Status::Warn | Status::Blocked)
}

pub async fn run_health(
    home: &Path,
    quiet: bool,
    degraded_only: bool,
    fail: bool,
    as_json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = HealthReport::new(crate::doctor::transport_findings(home).await);
    if as_json {
        println!("{}", serde_json::to_string(&report)?);
    } else if !quiet && (!degraded_only || report.requires_attention) {
        println!(
            "transport health: {} (daemon observations; not agent receipt)",
            if report.requires_attention {
                "attention required"
            } else {
                "observed"
            }
        );
        for finding in &report.findings {
            if !degraded_only || needs_attention(finding.status) {
                println!(
                    "{} {}: {}",
                    finding.status.label(),
                    finding.check,
                    finding.detail
                );
            }
        }
    }
    if report.requires_attention && (fail || quiet) {
        Err("transport observations require attention".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(status: Status) -> Finding {
        Finding {
            status,
            check: "test",
            detail: "observation".into(),
            fix: None,
        }
    }

    #[test]
    fn missing_or_partial_evidence_cannot_be_green() {
        assert!(HealthReport::new(vec![]).requires_attention);
        assert!(
            HealthReport::new(vec![finding(Status::Ok), finding(Status::Warn)]).requires_attention
        );
        assert!(HealthReport::new(vec![finding(Status::Blocked)]).requires_attention);
        assert!(
            !HealthReport::new(vec![finding(Status::Info), finding(Status::Ok)]).requires_attention
        );
    }
}
