//! End-to-end coverage for `airc-rs route ...`.

use std::process::Command;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

#[test]
fn route_status_defaults_to_local_runtime_route() {
    let output = run_ok(&["route", "status"]);

    assert!(output.contains("- local-fs role=direct state=healthy"));
    assert!(output.contains("- data-interactive -> local-fs"));
    assert!(output.contains("- presence-ephemeral -> local-fs"));
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
    let output = run_ok(&["route", "status", "--down", "local-fs:direct"]);

    assert!(output.contains("- local-fs role=direct state=down"));
    assert!(output.contains("- data-interactive -> no-route"));
}

fn run_ok(args: &[&str]) -> String {
    let output = Command::new(airc_rs())
        .args(args)
        .output()
        .expect("airc-rs command must spawn");
    assert!(
        output.status.success(),
        "airc-rs {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}
