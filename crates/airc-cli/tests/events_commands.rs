//! End-to-end coverage for `airc-core events ...`.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc-core")
}

#[test]
fn events_list_filters_by_kind_and_header_prefix() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "plain chat"]);
    run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "subscription filter proof",
        ],
    );

    let output = run_ok(
        &home,
        &[
            "events",
            "list",
            "--kind",
            "system",
            "--header-prefix",
            "forge.body_hint=forge.work.",
        ],
    );

    assert!(output.contains("events: 1"));
    assert!(output.contains("System"));
    assert!(output.contains("subscription filter proof"));
    assert!(!output.contains("plain chat"));
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let output = Command::new(airc_core())
        .arg("--home")
        .arg(home)
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
