//! End-to-end coverage for `airc-rs route ...`.

use std::process::Command;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

#[test]
fn route_status_defaults_to_local_runtime_route() {
    let output = run_ok(&["route", "status"]);

    assert!(output.contains("- local-fs role=direct state=healthy"));
    assert!(output.contains("- data -> local-fs"));
    assert!(output.contains("- live-event -> local-fs"));
    assert!(!output.contains("gh-gist"));
}

#[test]
fn route_status_keeps_github_bootstrap_only() {
    let output = run_ok(&["route", "status", "--bootstrap", "gh-gist"]);

    assert!(output.contains("- gh-gist role=bootstrap-only state=healthy"));
    assert!(output.contains("- data -> no-route"));
    assert!(output.contains("- live-event -> no-route"));
    assert!(output.contains("- bootstrap -> gh-gist"));
    assert!(output.contains("- migration -> gh-gist"));
}

#[test]
fn route_status_prefers_direct_route_over_bootstrap_github() {
    let output = run_ok(&[
        "route",
        "status",
        "--bootstrap",
        "gh-gist",
        "--direct",
        "reticulum",
    ]);

    assert!(output.contains("- data -> reticulum"));
    assert!(output.contains("- bootstrap -> reticulum"));
}

#[test]
fn route_status_down_override_removes_candidate() {
    let output = run_ok(&["route", "status", "--down", "local-fs:direct"]);

    assert!(output.contains("- local-fs role=direct state=down"));
    assert!(output.contains("- data -> no-route"));
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
