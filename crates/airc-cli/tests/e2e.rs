//! End-to-end integration test for the `airc-core` binary.
//!
//! Spawns two real subprocesses (Alice + Bob), has them chat over the
//! Rust substrate, and asserts the message arrives. No Python anywhere.
//!
//! Each subprocess uses its own `--home <dir>` so the identity state
//! is isolated per-test and per-peer.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc-core")
}

/// Run `airc-core --home <dir> init` and parse the printed `peer_id:`
/// and `peer_spec:` lines from stdout.
fn run_init(home: &Path) -> (String, String) {
    let output = Command::new(airc_core())
        .arg("--home")
        .arg(home)
        .arg("init")
        .output()
        .expect("airc-core init must spawn");
    assert!(
        output.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("init stdout utf-8");
    let peer_id = extract_field(&stdout, "peer_id:")
        .expect("init must print peer_id:")
        .to_string();
    let peer_spec = extract_field(&stdout, "peer_spec:")
        .expect("init must print peer_spec:")
        .to_string();
    (peer_id, peer_spec)
}

fn extract_field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
}

#[test]
fn two_airc_core_processes_chat_over_local_fs() {
    // Alice runs `airc-core listen`; Bob runs `airc-core send`; Alice's
    // stdout MUST contain the message body within a few seconds. No
    // Python anywhere.
    let workspace = TempDir::new().expect("tempdir");
    let alice_home = workspace.path().join("alice");
    let bob_home = workspace.path().join("bob");
    let wire = workspace.path().join("wire");

    let (_alice_id, alice_spec) = run_init(&alice_home);
    let (_bob_id, bob_spec) = run_init(&bob_home);

    // Both peers join the same room name — same channel UUID — and
    // override the wire to a shared dir so two local-fs processes
    // can talk. In production each peer has its own wire and uses
    // LAN-TCP / daemon, not a shared filesystem.
    run_room(&alice_home, "e2e", &wire);
    run_room(&bob_home, "e2e", &wire);

    let mut alice = Command::new(airc_core())
        .args([
            "--home",
            alice_home.to_str().unwrap(),
            "--peer",
            &bob_spec,
            "listen",
            "--replay",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("alice must spawn");

    std::thread::sleep(Duration::from_millis(300));

    let bob_send = Command::new(airc_core())
        .args([
            "--home",
            bob_home.to_str().unwrap(),
            "--peer",
            &alice_spec,
            "send",
            "hello from bob over rust substrate",
        ])
        .output()
        .expect("bob send must spawn");
    assert!(
        bob_send.status.success(),
        "bob send failed: stderr={}",
        String::from_utf8_lossy(&bob_send.stderr)
    );

    let alice_stdout = alice.stdout.take().expect("alice stdout");
    let body_arrived = wait_for_line_contains(
        alice_stdout,
        "hello from bob over rust substrate",
        Duration::from_secs(6),
    );

    let _ = alice.kill();
    let _ = alice.wait();

    assert!(
        body_arrived,
        "Alice's stdout did not contain Bob's message within 6s — substrate e2e is broken"
    );
}

/// Run `airc-core --home <dir> room <name> --wire <wire>`. Used by
/// tests to pin two peers to the same shared-wire room.
fn run_room(home: &Path, name: &str, wire: &Path) {
    let output = Command::new(airc_core())
        .args([
            "--home",
            home.to_str().unwrap(),
            "room",
            name,
            "--wire",
            wire.to_str().unwrap(),
        ])
        .output()
        .expect("airc-core room must spawn");
    assert!(
        output.status.success(),
        "room setup failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn join_without_args_uses_default_account_context() {
    let machine = TempDir::new().expect("tempdir");
    let repo = machine.path().join("continuum");
    let home = repo.join(".airc");
    create_repo_with_origin(&repo, "https://github.com/CambrianTech/continuum.git");
    seed_mesh_identity(&home, "joelteply");

    let output = Command::new(airc_core())
        .env("HOME", machine.path())
        .args(["--home", home.to_str().unwrap(), "join"])
        .current_dir(&repo)
        .output()
        .expect("airc-core join must spawn");
    assert!(
        output.status.success(),
        "join failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("#general"), "{stdout}");
    assert!(stdout.contains("#cambriantech"), "{stdout}");
    assert!(stdout.contains("default: #cambriantech"), "{stdout}");

    let subscriptions = std::fs::read_to_string(home.join("subscriptions.json")).unwrap();
    assert!(subscriptions.contains("\"general\""), "{subscriptions}");
    assert!(
        subscriptions.contains("\"cambriantech\""),
        "{subscriptions}"
    );
    let json: serde_json::Value = serde_json::from_str(&subscriptions).unwrap();
    let wire = json["subscribed"]["cambriantech"]["wire"]
        .as_str()
        .expect("cambriantech wire");
    assert_eq!(
        std::path::PathBuf::from(wire),
        machine
            .path()
            .join(".airc")
            .join("wires")
            .join("cambriantech")
    );
}

fn create_repo_with_origin(path: &Path, origin: &str) {
    std::fs::create_dir_all(path.join(".git")).unwrap();
    std::fs::write(
        path.join(".git/config"),
        format!(
            r#"[core]
    repositoryformatversion = 0
[remote "origin"]
    url = {origin}
"#
        ),
    )
    .unwrap();
}

fn seed_mesh_identity(home: &Path, identity: &str) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("mesh_identity.json"),
        format!(
            r#"{{
  "version": 1,
  "identity": "{identity}",
  "source": "operator",
  "resolved_at_ms": 1,
  "ttl_ms": 86400000
}}"#
        ),
    )
    .unwrap();
}

#[test]
fn listen_rejects_unenrolled_signer() {
    // Mallory's signed frame must be rejected by Alice's listen — she
    // isn't in Alice's peer registry. Stderr surfaces a verification
    // failure; stdout never prints it as a happy message.
    let workspace = TempDir::new().expect("tempdir");
    let alice_home = workspace.path().join("alice");
    let mallory_home = workspace.path().join("mallory");
    let wire = workspace.path().join("wire");

    let (_alice_id, _alice_spec) = run_init(&alice_home);
    let (_mallory_id, mallory_spec) = run_init(&mallory_home);

    // Both peers share the same wire / room name. Alice's peer
    // registry deliberately does NOT include Mallory.
    run_room(&alice_home, "e2e", &wire);
    run_room(&mallory_home, "e2e", &wire);

    let mut alice = Command::new(airc_core())
        .args(["--home", alice_home.to_str().unwrap(), "listen", "--replay"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("alice must spawn");

    std::thread::sleep(Duration::from_millis(300));

    // Mallory passes herself as `--peer` so her own registry is
    // non-empty; her CLI signs the frame under her own identity.
    let _mallory_send = Command::new(airc_core())
        .args([
            "--home",
            mallory_home.to_str().unwrap(),
            "--peer",
            &mallory_spec,
            "send",
            "should be rejected",
        ])
        .output()
        .expect("mallory send must spawn");

    let alice_stderr = alice.stderr.take().expect("alice stderr");
    let saw_rejection =
        wait_for_line_contains(alice_stderr, "verification failed", Duration::from_secs(6));

    let _ = alice.kill();
    let _ = alice.wait();

    assert!(
        saw_rejection,
        "Alice's listen should have surfaced a verification failure for Mallory's unenrolled frame"
    );
}

#[test]
fn two_airc_core_processes_chat_over_lan_via_sdk_route() {
    // The LAN CLI commands must be thin wrappers over airc-lib. This
    // test spawns real processes and uses `lan-listen` / `lan-send`
    // without a shared local-fs wire.
    let workspace = TempDir::new().expect("tempdir");
    let alice_home = workspace.path().join("alice");
    let bob_home = workspace.path().join("bob");

    let (alice_id, alice_spec) = run_init(&alice_home);
    let (_bob_id, bob_spec) = run_init(&bob_home);

    let mut alice = Command::new(airc_core())
        .args([
            "--home",
            alice_home.to_str().unwrap(),
            "--peer",
            &bob_spec,
            "lan-listen",
            "--bind",
            "127.0.0.1:0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("alice lan-listen must spawn");

    let alice_stdout = alice.stdout.take().expect("alice stdout");
    let lines = spawn_line_reader(alice_stdout);
    let listen_line =
        wait_for_channel_line_contains(&lines, "listening on", Duration::from_secs(6))
            .expect("alice must print bound LAN address");
    let bound_addr = parse_listening_addr(&listen_line);

    let bob_send = Command::new(airc_core())
        .args([
            "--home",
            bob_home.to_str().unwrap(),
            "--peer",
            &alice_spec,
            "lan-send",
            "--to",
            &bound_addr,
            "--expected-peer",
            &alice_id,
            "hello over cli sdk lan",
        ])
        .output()
        .expect("bob lan-send must spawn");
    assert!(
        bob_send.status.success(),
        "bob lan-send failed: stdout={} stderr={}",
        String::from_utf8_lossy(&bob_send.stdout),
        String::from_utf8_lossy(&bob_send.stderr),
    );

    let body_arrived =
        wait_for_channel_line_contains(&lines, "hello over cli sdk lan", Duration::from_secs(6))
            .is_some();

    let _ = alice.kill();
    let _ = alice.wait();

    assert!(
        body_arrived,
        "Alice's LAN listener did not print Bob's message within 6s"
    );
}

#[test]
fn daemon_msg_and_inbox_use_sdk_attach_path() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");
    let socket = workspace.path().join("daemon.sock");
    run_init(&home);

    let mut daemon = Command::new(airc_core())
        .args([
            "--home",
            home.to_str().unwrap(),
            "daemon",
            "--socket",
            socket.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daemon must spawn");

    wait_for_line_contains(
        daemon.stdout.take().expect("daemon stdout"),
        "listening on",
        Duration::from_secs(6),
    );

    let msg = Command::new(airc_core())
        .args([
            "--home",
            home.to_str().unwrap(),
            "msg",
            "--socket",
            socket.to_str().unwrap(),
            "hello through cli attach",
        ])
        .output()
        .expect("msg must spawn");
    assert!(
        msg.status.success(),
        "msg failed: stdout={} stderr={}",
        String::from_utf8_lossy(&msg.stdout),
        String::from_utf8_lossy(&msg.stderr),
    );

    let inbox = wait_for_command_stdout_contains(
        &home,
        &socket,
        "hello through cli attach",
        Duration::from_secs(6),
    );
    assert!(
        inbox,
        "inbox did not print daemon-sent message through SDK attach path"
    );

    let stop = Command::new(airc_core())
        .args([
            "--home",
            home.to_str().unwrap(),
            "stop",
            "--socket",
            socket.to_str().unwrap(),
        ])
        .output()
        .expect("stop must spawn");
    assert!(stop.status.success(), "stop failed");
    let _ = daemon.wait();
}

#[test]
fn inbox_without_socket_reads_in_process_store() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");
    run_init(&home);

    let send = Command::new(airc_core())
        .args([
            "--home",
            home.to_str().unwrap(),
            "send",
            "hello through in-process inbox",
        ])
        .output()
        .expect("send must spawn");
    assert!(
        send.status.success(),
        "send failed: stdout={} stderr={}",
        String::from_utf8_lossy(&send.stdout),
        String::from_utf8_lossy(&send.stderr),
    );

    let inbox = Command::new(airc_core())
        .args(["--home", home.to_str().unwrap(), "inbox", "--limit", "16"])
        .output()
        .expect("inbox must spawn");
    assert!(
        inbox.status.success(),
        "inbox failed: stdout={} stderr={}",
        String::from_utf8_lossy(&inbox.stdout),
        String::from_utf8_lossy(&inbox.stderr),
    );
    assert!(
        String::from_utf8_lossy(&inbox.stdout).contains("hello through in-process inbox"),
        "inbox stdout missing sent message: {}",
        String::from_utf8_lossy(&inbox.stdout)
    );
}

#[test]
fn rerun_init_returns_same_peer_id() {
    // The persistence pin: `airc-core init` must be idempotent. The
    // second run reuses the on-disk identity rather than minting a
    // new peer_id.
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("alice");
    let (first_id, first_spec) = run_init(&home);
    let (second_id, second_spec) = run_init(&home);
    assert_eq!(first_id, second_id, "peer_id must persist across init runs");
    assert_eq!(
        first_spec, second_spec,
        "peer_spec must persist across init runs"
    );
}

fn wait_for_command_stdout_contains(
    home: &Path,
    socket: &Path,
    needle: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let output = Command::new(airc_core())
            .args([
                "--home",
                home.to_str().unwrap(),
                "inbox",
                "--socket",
                socket.to_str().unwrap(),
                "--limit",
                "16",
            ])
            .output()
            .expect("inbox must spawn");
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(needle) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

fn parse_listening_addr(line: &str) -> String {
    line.strip_prefix("listening on ")
        .and_then(|rest| rest.split_whitespace().next())
        .expect("listening line must contain address")
        .to_string()
}

/// Block until `reader` yields a line containing `needle`, or the
/// deadline elapses.
fn wait_for_line_contains<R: Read + Send + 'static>(
    reader: R,
    needle: &str,
    timeout: Duration,
) -> bool {
    let needle = needle.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines().map_while(Result::ok) {
            if line.contains(&needle) {
                let _ = tx.send(());
                return;
            }
        }
    });
    let deadline = Instant::now() + timeout;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => return true,
            Err(_) => {
                if Instant::now() >= deadline {
                    return false;
                }
            }
        }
    }
}

// Trait keepalive for `Write` — silences unused-import lints if the
// import is otherwise only needed transitively.
#[allow(dead_code)]
fn _trait_keepalive(mut w: impl Write) {
    let _ = w.write(&[]);
}
