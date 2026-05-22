//! End-to-end coverage for `airc transport ...`.

use std::process::Command;

use tempfile::TempDir;

fn airc() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn transport_health_reports_route_snapshot_from_substrate() {
    let workspace = TempDir::new().expect("tempdir");

    let output = run_ok(workspace.path(), &["transport", "health"]);

    assert!(output.contains("transport health: ok (1 route(s) healthy)"));
    assert!(output.contains("- local-fs role=direct state=healthy"));
    assert!(output.contains("endpoints: none"));
    assert!(output.contains("lan peers: none"));
}

#[test]
fn transport_health_degraded_only_is_silent_when_routes_are_clean() {
    let workspace = TempDir::new().expect("tempdir");

    let output = run_ok(
        workspace.path(),
        &["transport", "health", "--degraded-only"],
    );

    assert!(output.trim().is_empty());
}

#[test]
fn transport_health_quiet_succeeds_when_routes_are_clean() {
    let workspace = TempDir::new().expect("tempdir");

    let output = run_raw(
        workspace.path(),
        &["transport", "health", "--quiet", "--fail"],
    );

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

fn run_ok(home: &std::path::Path, args: &[&str]) -> String {
    let output = run_raw(home, args);
    assert!(
        output.status.success(),
        "airc {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn run_raw(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(airc())
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("airc command must spawn")
}
