//! End-to-end coverage for `airc-core workspace ...`.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn workspace_request_allocate_heartbeat_release_projects_on_list() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let card = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "workspace cli lifecycle",
        ],
    );
    let card_id = extract_field(&card, "card_id:").expect("create prints card_id");
    let claim = run_ok(&home, &["work", "claim", card_id, "--ttl-ms", "60000"]);
    let claim_id = extract_field(&claim, "claim_id:").expect("claim prints claim_id");

    let request = run_ok(
        &home,
        &[
            "workspace",
            "request",
            card_id,
            claim_id,
            "--repo",
            "CambrianTech/airc",
            "--branch",
            "feat/workspace-commands",
            "--base",
            "rust-rewrite",
        ],
    );
    let workspace_id =
        extract_field(&request, "workspace_id:").expect("request prints workspace_id");

    run_ok(
        &home,
        &[
            "workspace",
            "allocate",
            workspace_id,
            "--path",
            "/tmp/airc/ws",
        ],
    );
    run_ok(
        &home,
        &[
            "workspace",
            "heartbeat",
            workspace_id,
            "--disk-bytes",
            "4096",
        ],
    );
    let active = run_ok(&home, &["workspace", "list"]);
    assert!(active.contains(workspace_id));
    assert!(active.contains("Active"));
    assert!(active.contains("disk_bytes=4096"));
    assert!(active.contains("feat/workspace-commands"));

    run_ok(&home, &["workspace", "release", workspace_id]);
    let released = run_ok(&home, &["workspace", "list"]);
    assert!(released.contains("Released"));
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

fn extract_field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
}
