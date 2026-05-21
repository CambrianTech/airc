//! End-to-end coverage for `airc-core worktree-lane ...`.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc-core")
}

#[test]
fn worktree_lane_registry_round_trips_from_cli() {
    let workspace = TempDir::new().expect("tempdir");
    let registry = workspace.path().join("lanes.jsonl");

    run_ok(
        &[
            "worktree-lane",
            "record",
            "--registry",
            registry.to_str().unwrap(),
            "--issue",
            "#123",
            "--repo",
            "/repo",
            "--dir",
            "/tmp/repo-123-codex",
            "--branch",
            "feat/123-codex",
            "--base",
            "origin/canary",
            "--owner",
            "codex",
        ],
        None,
    );

    let list = run_ok(
        &[
            "worktree-lane",
            "list",
            "--registry",
            registry.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    let parsed: Value = serde_json::from_str(&list).expect("list json");
    assert_eq!(parsed["lanes"][0]["issue"], "#123");
    assert_eq!(parsed["lanes"][0]["owner"], "codex");

    let dir = run_ok(
        &[
            "worktree-lane",
            "find",
            "--registry",
            registry.to_str().unwrap(),
            "repo-123-codex",
            "--field",
            "dir",
        ],
        None,
    );
    assert_eq!(dir.trim(), "/tmp/repo-123-codex");
}

#[test]
fn worktree_lane_slug_and_abs_path_match_shell_contract() {
    let workspace = TempDir::new().expect("tempdir");
    let cwd = workspace.path();

    let slug = run_ok(&["worktree-lane", "slug", "#123: Codex Lane!"], None);
    assert_eq!(slug.trim(), "123-codex-lane");

    let abs = run_ok(&["worktree-lane", "abs-path", "relative/lane"], Some(cwd));
    let abs_path = Path::new(abs.trim());
    assert!(abs_path.is_absolute(), "abs-path output was {abs}");
    assert!(
        abs_path.ends_with(Path::new("relative").join("lane")),
        "abs-path output was {abs}"
    );
}

fn run_ok(args: &[&str], cwd: Option<&Path>) -> String {
    let mut command = Command::new(airc_core());
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().expect("airc-core command must spawn");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}
