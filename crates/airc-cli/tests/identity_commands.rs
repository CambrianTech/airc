use std::process::Command;

use base64::{engine::general_purpose, Engine};
use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn identity_pretty_matches_whois_shape() {
    let output = Command::new(airc_core())
        .args([
            "identity",
            "pretty",
            "--name",
            "alice",
            "--identity-json",
            r#"{"pronouns":"she","role":"builder","bio":"makes things","status":"away","integrations":{"continuum":"alice-c"}}"#,
            "--host",
            "alice.local",
        ])
        .output()
        .expect("airc-core identity pretty must spawn");

    assert!(
        output.status.success(),
        "identity pretty failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        concat!(
            "  name:      alice\n",
            "  pronouns:   she\n",
            "  role:       builder\n",
            "  bio:        makes things\n",
            "  status:     away\n",
            "  integrations:\n",
            "    continuum: alice-c\n",
            "  host:      alice.local\n",
        )
    );
}

#[test]
fn identity_pretty_defaults_unset_fields() {
    let output = Command::new(airc_core())
        .args(["identity", "pretty", "--name", "alice"])
        .output()
        .expect("airc-core identity pretty must spawn");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("  pronouns:   (unset)\n"));
    assert!(stdout.contains("  integrations: (none)\n"));
}

#[test]
fn identity_set_link_and_show_round_trip_through_store() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();

    run_ok(
        home,
        &[
            "identity",
            "set",
            "--pronouns",
            "they/them",
            "--role",
            "rust-cutter",
            "--bio",
            "moves runtime identity into the ORM store",
            "--status",
            "focused",
        ],
        "",
    );
    run_ok(
        home,
        &[
            "identity",
            "link",
            "--platform",
            "continuum",
            "--handle",
            "clio",
        ],
        "",
    );

    let shown = run_ok(home, &["identity", "show"], "");
    assert!(shown.contains("  pronouns:   they/them\n"));
    assert!(shown.contains("  role:       rust-cutter\n"));
    assert!(shown.contains("  bio:        moves runtime identity into the ORM store\n"));
    assert!(shown.contains("  status:     focused\n"));
    assert!(shown.contains("    continuum: clio\n"));

    assert_eq!(
        run_ok(home, &["identity", "continuum-handle"], ""),
        "clio\n"
    );
}

#[test]
fn identity_import_continuum_merges_into_store_without_clearing_status() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();

    run_ok(
        home,
        &["identity", "set", "--status", "already-present"],
        "",
    );
    run_ok(
        home,
        &[
            "identity",
            "import-continuum",
            "--blob",
            r#"{"name":"delphi","pronouns":"she/her","role":"planner","bio":"keeps the lane coherent"}"#,
        ],
        "",
    );

    let shown = run_ok(home, &["identity", "show"], "");
    assert!(shown.contains("  pronouns:   she/her\n"));
    assert!(shown.contains("  role:       planner\n"));
    assert!(shown.contains("  bio:        keeps the lane coherent\n"));
    assert!(shown.contains("  status:     already-present\n"));
    assert!(shown.contains("    continuum: delphi\n"));
}

#[test]
fn legacy_identity_commands_bootstrap_lookup_and_sign() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    let identity_dir = home.join("identity");
    let peers_dir = home.join("peers");
    std::fs::create_dir_all(&peers_dir).unwrap();

    let x25519 = run_ok(
        home,
        &[
            "identity",
            "bootstrap",
            "--dir",
            identity_dir.to_str().unwrap(),
        ],
        "",
    );
    assert_eq!(
        general_purpose::URL_SAFE_NO_PAD
            .decode(x25519.trim())
            .unwrap()
            .len(),
        32
    );

    std::fs::write(
        peers_dir.join("alice.json"),
        format!(r#"{{"x25519_pub":"{}"}}"#, x25519.trim()),
    )
    .unwrap();
    assert_eq!(
        run_ok(
            home,
            &[
                "identity",
                "peer-pub",
                "--peers-dir",
                peers_dir.to_str().unwrap(),
                "--peer-name",
                "alice",
            ],
            "",
        ),
        x25519
    );

    run_ok(
        home,
        &[
            "identity",
            "bootstrap-ed25519",
            "--dir",
            identity_dir.to_str().unwrap(),
        ],
        "",
    );
    let signature = run_ok(
        home,
        &[
            "identity",
            "sign-ed25519",
            "--dir",
            identity_dir.to_str().unwrap(),
        ],
        "hello",
    );
    assert_eq!(
        general_purpose::STANDARD
            .decode(signature.trim())
            .unwrap()
            .len(),
        64
    );
}

#[test]
fn legacy_envelope_wrap_encrypts_message_field() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    let alice = home.join("alice");
    let bob = home.join("bob");
    let bob_pub = run_ok(
        home,
        &["identity", "bootstrap", "--dir", bob.to_str().unwrap()],
        "",
    );
    run_ok(
        home,
        &["identity", "bootstrap", "--dir", alice.to_str().unwrap()],
        "",
    );

    let wrapped = run_ok(
        home,
        &[
            "envelope",
            "wrap",
            "--identity-dir",
            alice.to_str().unwrap(),
            &format!("--recipient-pub={}", bob_pub.trim()),
        ],
        r#"{"from":"alice","to":"bob","ts":"2026-05-19T00:00:00Z","channel":"general","msg":"hello"}"#,
    );
    let value: serde_json::Value = serde_json::from_str(&wrapped).unwrap();
    assert_eq!(
        value.get("enc").and_then(serde_json::Value::as_str),
        Some("v1")
    );
    assert_ne!(
        value.get("msg").and_then(serde_json::Value::as_str),
        Some("hello")
    );
    assert!(value
        .get("nonce")
        .and_then(serde_json::Value::as_str)
        .is_some());
}

fn run_ok(home: &std::path::Path, args: &[&str], stdin: &str) -> String {
    use std::io::Write;
    let mut child = Command::new(airc_core())
        .arg("--home")
        .arg(home)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("airc-core must spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("airc-core must exit");
    assert!(
        output.status.success(),
        "airc-core failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).unwrap()
}
