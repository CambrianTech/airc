//! End-to-end coverage for `airc transport ...`.

use std::process::Command;

mod common;

fn airc() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn transport_health_reports_no_routes_on_fresh_scope() {
    let workspace = common::daemon_tempdir();

    let output = run_ok(workspace.path(), &["transport", "health"]);

    // Card 9cbe1101 (seam #6): a fresh scope's daemon has registered
    // ZERO transports — same-machine delivery is the in-memory router,
    // not a counted route. Before the verdict refactor this scope
    // surfaced as `ok (0 route(s) healthy)`, which is the lie this PR
    // killed. The honest verdict is `no-routes` — and it must NEVER
    // render as `ok`, since `ok` paints a substrate-not-routing state
    // as healthy to operators.
    assert!(
        output.contains("transport health: no-routes"),
        "expected no-routes verdict, got: {output}"
    );
    assert!(
        !output.contains("transport health: ok"),
        "no-routes must not render as ok (the live-found seam-#6 lie): {output}"
    );
    assert!(!output.contains("local-fs"));
    assert!(output.contains("endpoints: none"));
    assert!(output.contains("lan peers: none"));
}

#[test]
fn transport_health_degraded_only_is_silent_when_routes_are_clean() {
    let workspace = common::daemon_tempdir();

    let output = run_ok(
        workspace.path(),
        &["transport", "health", "--degraded-only"],
    );

    assert!(output.trim().is_empty());
}

#[test]
fn transport_health_quiet_fail_exits_nonzero_on_no_routes() {
    // Card 9cbe1101 (seam #6): a fresh scope has no routes — the
    // typed verdict is `NoRoutes`, which `is_failure() == true`, so
    // `--fail` must exit nonzero. Before the verdict refactor this
    // path returned success on 0 healthy routes — which is exactly
    // the operator-misleading behavior we're killing.
    let workspace = common::daemon_tempdir();

    let output = run_raw(
        workspace.path(),
        &["transport", "health", "--quiet", "--fail"],
    );

    assert!(
        !output.status.success(),
        "quiet+fail must exit nonzero when verdict is no-routes: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
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
