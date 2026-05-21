//! End-to-end coverage for `airc-core transport ...`.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn transport_health_reports_fresh_heartbeat() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    let gist = "c68640ec0144b422c16b2d8c83ad5ee5";
    write_config(home, gist);
    fs::write(
        home.join("bearer_state.general.json"),
        format!(r#"{{"last_heartbeat_ts":{}}}"#, now_seconds()),
    )
    .unwrap();
    fs::write(
        home.join(format!("bearer_gist.{gist}.pid")),
        std::process::id().to_string(),
    )
    .unwrap();

    let output = run_ok(home, &["transport", "health"]);

    assert!(output.contains("transport health: ok (1 channel(s) fresh)"));
    assert!(output.contains("#general: ok"));
}

#[test]
fn transport_health_fail_exits_nonzero_when_degraded() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    let gist = "c68640ec0144b422c16b2d8c83ad5ee5";
    write_config(home, gist);
    fs::write(
        home.join("bearer_state.general.json"),
        r#"{"last_heartbeat_ts":700}"#,
    )
    .unwrap();
    fs::write(home.join(format!("bearer_gist.{gist}.pid")), "999999").unwrap();

    let output = run_raw(
        home,
        &["transport", "health", "--fresh-after", "90", "--fail"],
    );

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("transport health: DEGRADED"));
    assert!(stdout.contains("stale heartbeat"));
}

#[test]
fn transport_health_default_reports_degraded_without_failing() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    let gist = "c68640ec0144b422c16b2d8c83ad5ee5";
    write_config(home, gist);

    let output = run_ok(home, &["transport", "health"]);

    assert!(output.contains("transport health: DEGRADED"));
    assert!(output.contains("no bearer_state file"));
}

#[test]
fn transport_health_degraded_only_is_silent_when_clean() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    let gist = "c68640ec0144b422c16b2d8c83ad5ee5";
    write_config(home, gist);
    fs::write(
        home.join("bearer_state.general.json"),
        format!(r#"{{"last_heartbeat_ts":{}}}"#, now_seconds()),
    )
    .unwrap();
    fs::write(
        home.join(format!("bearer_gist.{gist}.pid")),
        std::process::id().to_string(),
    )
    .unwrap();

    let output = run_ok(home, &["transport", "health", "--degraded-only"]);

    assert!(output.trim().is_empty());
}

fn write_config(home: &Path, gist: &str) {
    fs::write(
        home.join("config.json"),
        format!(r#"{{"subscribed_channels":["general"],"channel_gists":{{"general":"{gist}"}}}}"#),
    )
    .unwrap();
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let output = run_raw(home, args);
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn run_raw(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(airc_core())
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn")
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
