//! End-to-end coverage for `airc inbox --json`.
//!
//! Closes work card 3a069fab: machine consumers can't reliably
//! parse `airc inbox`'s human text output. The `--json` flag
//! gives them a stable shape that `jq` parses without surprises.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn inbox_json_returns_count_events_cursor_shape() {
    // Just asserts the shape — the bare `airc init` flow may
    // emit lifecycle events (RoomJoined, presence beacons)
    // that land in inbox, so we don't pin a specific count.
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let stdout = run_ok(&home, &["inbox", "--json"]);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be JSON");

    assert!(value["count"].is_number(), "count missing: {value}");
    assert!(value["events"].is_array(), "events array missing: {value}");

    let count = value["count"].as_u64().expect("count is number");
    let events_len = value["events"].as_array().expect("events array").len() as u64;
    assert_eq!(count, events_len, "count must equal events.len()");

    // Cursor present iff at least one event. JSON null vs object
    // is the discriminator — never absent.
    if count == 0 {
        assert!(
            value["cursor"].is_null(),
            "empty inbox must have null cursor"
        );
    } else {
        let cursor = &value["cursor"];
        assert!(cursor.is_object(), "non-empty cursor must be an object");
        assert!(cursor["lamport"].is_number());
        assert!(cursor["event_id"].is_string());
    }
}

#[test]
fn inbox_json_carries_events_plus_paging_cursor() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    // Generate some events. `airc send` lands a Message in the
    // default room which inbox will surface.
    run_ok(&home, &["send", "hello-1"]);
    run_ok(&home, &["send", "hello-2"]);
    run_ok(&home, &["send", "hello-3"]);

    let stdout = run_ok(&home, &["inbox", "--json"]);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be JSON");

    let count = value["count"].as_u64().expect("count number");
    assert!(count >= 3, "expected >=3 events, got {count}: {value}");

    let events = value["events"].as_array().expect("events array");
    assert_eq!(
        events.len(),
        count as usize,
        "count must match events.len()"
    );

    // Cursor shape: {lamport, event_id}.
    let cursor = &value["cursor"];
    assert!(cursor.is_object(), "cursor must be object, got {cursor}");
    assert!(cursor["lamport"].is_number(), "cursor.lamport must exist");
    assert!(
        cursor["event_id"].is_string(),
        "cursor.event_id must be a string UUID"
    );
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let account_home = home.parent().unwrap_or(home);
    let output = Command::new(airc_core())
        .arg("--home")
        .arg(home)
        .args(args)
        .env("HOME", account_home)
        .env("USERPROFILE", account_home)
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
