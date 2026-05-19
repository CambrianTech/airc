//! End-to-end integration test for the `airc-rs` binary.
//!
//! Spawns two real subprocesses (Alice + Bob), has them chat over the
//! Rust substrate, and asserts the message arrives. This is the proof
//! of life: no Python anywhere in the loop. If this test passes,
//! Python `airc` has a fully functional Rust replacement for the
//! basic chat use case.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Returns the path to the `airc-rs` binary cargo built for this test.
fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

/// Run `airc-rs init` and parse the printed `peer_id:` and
/// `peer_spec:` lines from stdout.
fn run_init(identity_file: &Path) -> (String, String) {
    let output = Command::new(airc_rs())
        .arg("--identity-file")
        .arg(identity_file)
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
    // The headline test: Alice runs `airc-rs listen`; Bob runs
    // `airc-rs send`; Alice's stdout MUST contain the message body
    // within a few seconds. No Python anywhere.
    let dir = TempDir::new().expect("tempdir");
    let alice_key = dir.path().join("alice.key");
    let bob_key = dir.path().join("bob.key");
    let wire = dir.path().join("wire");

    let (alice_id, alice_spec) = run_init(&alice_key);
    let (bob_id, bob_spec) = run_init(&bob_key);

    // Fixed channel UUID so both sides agree.
    let channel = "11111111-2222-3333-4444-555555555555";

    // Spawn Alice's listener with --replay so she sees messages sent
    // even slightly before her subscribe completes (race-safe).
    let mut alice = Command::new(airc_rs())
        .args([
            "--identity-file",
            alice_key.to_str().unwrap(),
            "--peer-id",
            &alice_id,
            "--peer",
            &bob_spec,
            "listen",
            "--wire",
            wire.to_str().unwrap(),
            "--channel",
            channel,
            "--replay",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("alice must spawn");

    // Give the listener a moment to start the tail loop.
    std::thread::sleep(Duration::from_millis(300));

    // Bob sends.
    let bob_send = Command::new(airc_rs())
        .args([
            "--identity-file",
            bob_key.to_str().unwrap(),
            "--peer-id",
            &bob_id,
            "--peer",
            &alice_spec,
            "send",
            "--wire",
            wire.to_str().unwrap(),
            "--channel",
            channel,
            "hello from bob over rust substrate",
        ])
        .output()
        .expect("bob send must spawn");
    assert!(
        bob_send.status.success(),
        "bob send failed: stderr={}",
        String::from_utf8_lossy(&bob_send.stderr)
    );

    // Read Alice's stdout until we see the message or hit the timeout.
    let alice_stdout = alice.stdout.take().expect("alice stdout");
    let body_arrived = wait_for_line_contains(
        alice_stdout,
        "hello from bob over rust substrate",
        Duration::from_secs(6),
    );

    // Clean up: kill the listener.
    let _ = alice.kill();
    let _ = alice.wait();

    assert!(
        body_arrived,
        "Alice's stdout did not contain Bob's message within 6s — substrate e2e is broken"
    );
}

#[test]
fn listen_rejects_unenrolled_signer() {
    // Security guarantee through the CLI: Mallory (not in Alice's
    // peer list) writes a signed frame to the wire. Alice's listen
    // process should NOT print it as a happy message; instead it
    // surfaces "verification failed" via stderr.
    let dir = TempDir::new().expect("tempdir");
    let alice_key = dir.path().join("alice.key");
    let mallory_key = dir.path().join("mallory.key");
    let wire = dir.path().join("wire");

    let (alice_id, _alice_spec) = run_init(&alice_key);
    let (mallory_id, mallory_spec) = run_init(&mallory_key);

    let channel = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    // Alice's peer list does NOT include Mallory.
    // No `--peer` flags passed at all.
    let mut alice = Command::new(airc_rs())
        .args([
            "--identity-file",
            alice_key.to_str().unwrap(),
            "--peer-id",
            &alice_id,
            "listen",
            "--wire",
            wire.to_str().unwrap(),
            "--channel",
            channel,
            "--replay",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("alice must spawn");

    std::thread::sleep(Duration::from_millis(300));

    // Mallory sends; Mallory's registry only contains herself + Alice
    // (so she CAN sign), but Alice's registry doesn't contain her.
    let _mallory_send = Command::new(airc_rs())
        .args([
            "--identity-file",
            mallory_key.to_str().unwrap(),
            "--peer-id",
            &mallory_id,
            "--peer",
            // Mallory needs Alice in HER registry to sign — but the
            // CLI's send only needs the sender's own identity to be
            // enrolled (we enrol self automatically). We still pass
            // Alice as a peer so the registry is non-empty.
            &mallory_spec,
            "send",
            "--wire",
            wire.to_str().unwrap(),
            "--channel",
            channel,
            "should be rejected",
        ])
        .output()
        .expect("mallory send must spawn");

    // Read Alice's stderr — we expect a "verification failed" line.
    // (Stdout might be empty because the listen never accepts the
    // frame as a valid message.)
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

/// Block until `reader` yields a line containing `needle`, or the
/// deadline elapses. Reads byte-by-byte so we don't have to predict
/// line boundaries; this matters because the airc-rs output is
/// line-oriented but the test's view of stdout/stderr is a stream.
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

// Linker stub: silences an unused-import lint when `Write` is only
// pulled in for trait usage. (`Write` is used implicitly by `tx.send`
// via the channel impl; this trait import is here in case future
// tests need it directly.)
#[allow(dead_code)]
fn _trait_keepalive(mut w: impl Write) {
    let _ = w.write(&[]);
}
