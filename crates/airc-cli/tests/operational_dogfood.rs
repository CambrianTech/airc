//! Operational dogfood proof for installed agent identities.
//!
//! This test intentionally does NOT pass `--home` and does NOT pass
//! ad-hoc `--peer` flags to listen/send. Each subprocess receives a
//! distinct fake user HOME, so `airc-rs` resolves state through the
//! default installed path: `<HOME>/.airc`.
//!
//! The proof shape mirrors the real Codex/Claude target:
//!   1. each agent initialises its own installed identity
//!   2. each persists the other's peer spec
//!   3. both join the same room/wire
//!   4. both keep a live subscription open
//!   5. each sends and the other receives through persisted state

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

#[test]
fn installed_codex_and_claude_identities_hold_bidirectional_live_room() {
    let workspace = TempDir::new().expect("workspace tempdir");
    let codex_user_home = workspace.path().join("codex-user");
    let claude_user_home = workspace.path().join("claude-user");
    let shared_wire = workspace.path().join("shared-agent-wire");

    std::fs::create_dir_all(&codex_user_home).expect("codex user home");
    std::fs::create_dir_all(&claude_user_home).expect("claude user home");

    let codex_spec = installed_init(&codex_user_home).peer_spec;
    let claude_spec = installed_init(&claude_user_home).peer_spec;

    installed_peer_add(&codex_user_home, &claude_spec);
    installed_peer_add(&claude_user_home, &codex_spec);

    installed_room(&codex_user_home, "agent-dogfood", &shared_wire);
    installed_room(&claude_user_home, "agent-dogfood", &shared_wire);

    let mut codex_listener = installed_listen(&codex_user_home);
    let mut claude_listener = installed_listen(&claude_user_home);
    let codex_lines = spawn_line_reader(codex_listener.stdout.take().expect("codex stdout"));
    let claude_lines = spawn_line_reader(claude_listener.stdout.take().expect("claude stdout"));

    assert!(
        wait_for_channel_line_contains(&codex_lines, "listening on", Duration::from_secs(6))
            .is_some(),
        "codex listener did not start"
    );
    assert!(
        wait_for_channel_line_contains(&claude_lines, "listening on", Duration::from_secs(6))
            .is_some(),
        "claude listener did not start"
    );

    installed_send(
        &codex_user_home,
        "codex -> claude through installed .airc state",
    );
    let claude_saw_codex = wait_for_channel_line_contains(
        &claude_lines,
        "codex -> claude through installed .airc state",
        Duration::from_secs(6),
    )
    .is_some();
    assert!(
        claude_saw_codex,
        "claude listener did not receive codex send"
    );

    installed_send(
        &claude_user_home,
        "claude -> codex through installed .airc state",
    );
    let codex_saw_claude = wait_for_channel_line_contains(
        &codex_lines,
        "claude -> codex through installed .airc state",
        Duration::from_secs(6),
    )
    .is_some();
    assert!(
        codex_saw_claude,
        "codex listener did not receive claude send"
    );

    let codex_inbox = installed_inbox(&codex_user_home);
    assert!(
        codex_inbox.contains("claude -> codex through installed .airc state"),
        "codex persisted inbox did not contain claude send: {codex_inbox}"
    );
    let claude_inbox = installed_inbox(&claude_user_home);
    assert!(
        claude_inbox.contains("codex -> claude through installed .airc state"),
        "claude persisted inbox did not contain codex send: {claude_inbox}"
    );

    let _ = codex_listener.kill();
    let _ = claude_listener.kill();
    let _ = codex_listener.wait();
    let _ = claude_listener.wait();
}

struct InitOutput {
    peer_spec: String,
}

fn installed_init(user_home: &Path) -> InitOutput {
    let output = installed_command(user_home)
        .arg("init")
        .output()
        .expect("airc-rs init must spawn");
    assert!(
        output.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("init stdout utf-8");
    let home_line = extract_field(&stdout, "home:").expect("init prints home");
    assert!(
        home_line.ends_with(".airc"),
        "installed init must default to a .airc home, got {home_line}"
    );
    InitOutput {
        peer_spec: extract_field(&stdout, "peer_spec:")
            .expect("init prints peer_spec")
            .to_string(),
    }
}

fn installed_peer_add(user_home: &Path, peer_spec: &str) {
    let output = installed_command(user_home)
        .args(["peer", "add", peer_spec])
        .output()
        .expect("airc-rs peer add must spawn");
    assert!(
        output.status.success(),
        "peer add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn installed_room(user_home: &Path, room: &str, wire: &Path) {
    let output = installed_command(user_home)
        .args(["room", room, "--wire", wire.to_str().expect("wire utf-8")])
        .output()
        .expect("airc-rs room must spawn");
    assert!(
        output.status.success(),
        "room failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn installed_listen(user_home: &Path) -> std::process::Child {
    installed_command(user_home)
        .arg("listen")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc-rs listen must spawn")
}

fn installed_send(user_home: &Path, text: &str) {
    let output = installed_command(user_home)
        .args(["send", text])
        .output()
        .expect("airc-rs send must spawn");
    assert!(
        output.status.success(),
        "send failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn installed_inbox(user_home: &Path) -> String {
    let output = installed_command(user_home)
        .args(["inbox", "--limit", "16"])
        .output()
        .expect("airc-rs inbox must spawn");
    assert!(
        output.status.success(),
        "inbox failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("inbox stdout utf-8")
}

fn installed_command(user_home: &Path) -> Command {
    let mut command = Command::new(airc_rs());
    command.env("HOME", user_home);
    command.env("USERPROFILE", user_home);
    command
}

fn extract_field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
}

fn spawn_line_reader<R: Read + Send + 'static>(reader: R) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

fn wait_for_channel_line_contains(
    rx: &mpsc::Receiver<String>,
    needle: &str,
    timeout: Duration,
) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains(needle) => return Some(line),
            Ok(_) => {}
            Err(_) => {
                if Instant::now() >= deadline {
                    return None;
                }
            }
        }
    }
}
