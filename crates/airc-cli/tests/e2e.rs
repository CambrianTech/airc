//! End-to-end integration test for the `airc-rs` binary.
//!
//! Spawns two real subprocesses (Alice + Bob), has them chat over the
//! Rust substrate, and asserts the message arrives. No Python anywhere.
//!
//! Each subprocess uses its own `--home <dir>` so the identity state
//! is isolated per-test and per-peer.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

/// Run `airc-rs --home <dir> init` and parse the printed `peer_id:`
/// and `peer_spec:` lines from stdout.
fn run_init(home: &Path) -> (String, String) {
    let output = Command::new(airc_rs())
        .arg("--home")
        .arg(home)
        .arg("init")
        .output()
        .expect("airc-rs init must spawn");
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
fn two_airc_rs_processes_chat_over_local_fs() {
    // Alice runs `airc-rs listen`; Bob runs `airc-rs send`; Alice's
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

    let mut alice = Command::new(airc_rs())
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

    let bob_send = Command::new(airc_rs())
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

/// Run `airc-rs --home <dir> room <name> --wire <wire>`. Used by
/// tests to pin two peers to the same shared-wire room.
fn run_room(home: &Path, name: &str, wire: &Path) {
    let output = Command::new(airc_rs())
        .args([
            "--home",
            home.to_str().unwrap(),
            "room",
            name,
            "--wire",
            wire.to_str().unwrap(),
        ])
        .output()
        .expect("airc-rs room must spawn");
    assert!(
        output.status.success(),
        "room setup failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
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

    let mut alice = Command::new(airc_rs())
        .args(["--home", alice_home.to_str().unwrap(), "listen", "--replay"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("alice must spawn");

    std::thread::sleep(Duration::from_millis(300));

    // Mallory passes herself as `--peer` so her own registry is
    // non-empty; her CLI signs the frame under her own identity.
    let _mallory_send = Command::new(airc_rs())
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
fn rerun_init_returns_same_peer_id() {
    // The persistence pin: `airc-rs init` must be idempotent. The
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
