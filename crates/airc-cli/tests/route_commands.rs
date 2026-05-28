//! End-to-end coverage for `airc-core route ...`.

use std::process::Command;

use serde_json::Value;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn route_status_defaults_to_no_local_transport() {
    // Same-machine delivery is the daemon's in-memory router, not a
    // registered transport — a bare `route status` has no direct local
    // transport and no live route until a cross-machine one is added.
    let output = run_ok(&["route", "status"]);

    assert!(!output.contains("local-fs"));
    assert!(output.contains("- data-interactive -> no-route"));
    assert!(output.contains("- presence-ephemeral -> no-route"));
    assert!(!output.contains("gh-gist"));
}

#[test]
fn route_status_keeps_github_invite_only() {
    let output = run_ok(&["route", "status", "--invite", "gh-gist"]);

    assert!(output.contains("- gh-gist role=invite-beacon state=healthy"));
    assert!(output.contains("- invite-advertise -> gh-gist"));
    assert!(output.contains("- data-interactive -> no-route"));
    assert!(output.contains("- presence-ephemeral -> no-route"));
    assert!(!output.contains("migration"));
}

#[test]
fn route_status_prefers_direct_route_over_rendezvous_github() {
    let output = run_ok(&[
        "route",
        "status",
        "--rendezvous",
        "gh-gist",
        "--direct",
        "reticulum",
    ]);

    assert!(output.contains("- peer-rendezvous -> reticulum"));
    assert!(output.contains("- data-interactive -> reticulum"));
}

#[test]
fn route_status_down_override_removes_candidate() {
    let output = run_ok(&["route", "status", "--down", "lan-tcp:direct"]);

    assert!(output.contains("- lan-tcp role=direct state=down"));
    assert!(output.contains("- data-interactive -> no-route"));
}

#[test]
fn route_proof_lan_loopback_outputs_machine_readable_report() {
    let output = run_ok(&[
        "route",
        "proof",
        "--kind",
        "lan-loopback",
        "--timeout-ms",
        "3000",
    ]);
    let report: Value = serde_json::from_str(&output).expect("route proof output must be JSON");

    assert_eq!(report["proof"], "lan-loopback");
    assert_eq!(report["transport"], "lan-tcp");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["github_routine_traffic"], false);
    assert_eq!(report["reply_body"], "route-proof-pong");
    assert!(report["correlation_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
}

#[test]
fn route_proof_relay_loopback_outputs_machine_readable_report() {
    let output = run_ok(&[
        "route",
        "proof",
        "--kind",
        "relay-loopback",
        "--timeout-ms",
        "3000",
    ]);
    let report: Value = serde_json::from_str(&output).expect("route proof output must be JSON");

    assert_eq!(report["proof"], "relay-loopback");
    assert_eq!(report["transport"], "relay");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["github_routine_traffic"], false);
    assert_eq!(report["reply_body"], "route-proof-pong");
    assert!(report["relay_peer_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert!(report["relay_addr"]
        .as_str()
        .is_some_and(|addr| !addr.is_empty()));
}

fn run_ok(args: &[&str]) -> String {
    let output = Command::new(airc_core())
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}
