//! End-to-end coverage for `airc-rs work ...`.
//!
//! These tests execute the binary as a consumer would. The command
//! path must stay a thin wrapper over `airc-lib`, but the proof needs
//! to be from the CLI surface because that is what agents will use
//! while the daemon-attached API is still landing.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

#[test]
fn work_create_claim_release_projects_on_board() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);

    let create = run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "wire work commands through airc-lib",
            "--priority",
            "p1",
            "--body",
            "cli proof against the rust substrate",
        ],
    );
    let card_id = extract_field(&create, "card_id:").expect("create prints card_id");

    let board = run_ok(&home, &["work", "board"]);
    assert!(board.contains(card_id));
    assert!(board.contains("CambrianTech/airc"));
    assert!(board.contains("wire work commands through airc-lib"));
    assert!(board.contains("P1"));
    assert!(board.contains("Open"));

    let claim = run_ok(&home, &["work", "claim", card_id, "--ttl-ms", "60000"]);
    let claim_id = extract_field(&claim, "claim_id:").expect("claim prints claim_id");

    let claimed_board = run_ok(&home, &["work", "board"]);
    assert!(claimed_board.contains(card_id));
    assert!(claimed_board.contains(claim_id));
    assert!(claimed_board.contains("Claimed"));

    run_ok(
        &home,
        &[
            "work",
            "release",
            card_id,
            claim_id,
            "--reason",
            "merged into rust-rewrite",
        ],
    );

    let released_board = run_ok(&home, &["work", "board"]);
    assert!(released_board.contains(card_id));
    assert!(released_board.contains("Open"));
    assert!(released_board.contains("claim=-"));
}

#[test]
fn work_board_empty_state_is_explicit() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let board = run_ok(&home, &["work", "board"]);

    assert!(
        board.contains("(no work cards)"),
        "empty board should be explicit, got: {board}"
    );
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let output = Command::new(airc_rs())
        .arg("--home")
        .arg(home)
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

fn extract_field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
}
