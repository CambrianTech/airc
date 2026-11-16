use std::process::Command;

use base64::{engine::general_purpose, Engine};
mod common;

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

    common::assert_success(&output, "identity pretty");
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
fn init_as_records_agent_name() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path();

    let init = run_ok(home, &["init", "--as", "codex"], "");
    assert!(init.contains("agent_name:  codex\n"));

    let repeat = run_ok(home, &["init", "--as", "codex"], "");
    assert!(repeat.contains("agent_name:  codex\n"));
}

#[test]
fn init_uses_airc_agent_name_env_when_as_is_absent() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path();

    let output = Command::new(airc_core())
        .arg("--home")
        .arg(home)
        .arg("init")
        .env("AIRC_AGENT_NAME", "claude")
        .output()
        .expect("airc-core must spawn");

    common::assert_success(&output, "airc-core");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("agent_name:  claude\n"));
}

#[test]
fn identity_set_link_and_show_round_trip_through_store() {
    let workspace = common::daemon_tempdir().preserve_on_panic();
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

    // Regression: the selected named profile must be both the read and write
    // target. A partial update must not copy the default card into that row.
    run_ok_as(
        home,
        "alpha",
        &[
            "identity",
            "set",
            "--name",
            "Alpha display",
            "--pronouns",
            "she/her",
            "--role",
            "builder",
            "--bio",
            "alpha profile",
            "--status",
            "alpha-ready",
        ],
        "",
    );
    run_ok_as(
        home,
        "alpha",
        &[
            "identity",
            "link",
            "--platform",
            "continuum",
            "--handle",
            "alpha-c",
        ],
        "",
    );
    let alpha = run_ok_as(home, "alpha", &["identity", "show"], "");
    assert!(alpha.contains("  name:       Alpha display\n"), "{alpha}");
    assert!(alpha.contains("  pronouns:   she/her\n"));
    assert!(alpha.contains("  role:       builder\n"));
    assert!(alpha.contains("  bio:        alpha profile\n"));
    assert!(alpha.contains("  status:     alpha-ready\n"));
    assert!(alpha.contains("    continuum: alpha-c\n"));

    run_ok_as(
        home,
        "beta",
        &[
            "identity",
            "set",
            "--role",
            "reviewer",
            "--status",
            "beta-ready",
        ],
        "",
    );
    let beta = run_ok_as(home, "beta", &["identity", "show"], "");
    assert!(beta.contains("  name:       beta\n"), "{beta}");
    assert!(beta.contains("  pronouns:   (unset)\n"));
    assert!(beta.contains("  role:       reviewer\n"));
    assert!(beta.contains("  bio:        (unset)\n"));
    assert!(beta.contains("  status:     beta-ready\n"));
    assert!(beta.contains("  integrations: (none)\n"));

    run_ok_as(
        home,
        "alpha",
        &["identity", "set", "--status", "alpha-busy"],
        "",
    );
    let updated_alpha = run_ok_as(home, "alpha", &["identity", "show"], "");
    assert_eq!(updated_alpha, alpha.replace("alpha-ready", "alpha-busy"));
    assert_eq!(run_ok_as(home, "alpha", &["whois"], ""), updated_alpha);
    assert_eq!(
        run_ok_as(home, "alpha", &["identity", "continuum-handle"], ""),
        "alpha-c\n"
    );
    assert_eq!(run_ok_as(home, "beta", &["identity", "show"], ""), beta);
    assert_eq!(run_ok(home, &["identity", "show"], ""), shown);

    // Replace the old redirect-only unit test with actual sender evidence:
    // project and machine identities share transport, but own separate cards.
    let account = workspace.path().join("account");
    let machine = account.join(".airc");
    let project = account.join("project").join(".airc");
    let run_in_account = |scope: &std::path::Path, args: &[&str]| {
        run_ok_as_in_account(
            scope,
            airc_store::DEFAULT_AGENT_NAME,
            Some(&account),
            args,
            "",
        )
    };
    let machine_init = run_in_account(&machine, &["init"]);
    let project_init = run_in_account(&project, &["init"]);
    let peer_id = |output: &str| {
        output
            .lines()
            .find_map(|line| line.strip_prefix("peer_id:"))
            .expect("init peer_id")
            .trim()
            .parse::<uuid::Uuid>()
            .expect("peer UUID")
    };
    let machine_peer = peer_id(&machine_init);
    let project_peer = peer_id(&project_init);
    assert_ne!(machine_peer, project_peer);
    let machine_key = std::fs::read(machine.join("identity.key")).unwrap();
    let project_key = std::fs::read(project.join("identity.key")).unwrap();
    run_in_account(
        &machine,
        &[
            "identity",
            "set",
            "--name",
            "Machine profile",
            "--status",
            "machine-ready",
        ],
    );
    let machine_card = run_in_account(&machine, &["identity", "show"]);
    run_in_account(
        &project,
        &[
            "identity",
            "set",
            "--name",
            "Project profile",
            "--status",
            "project-ready",
        ],
    );
    run_in_account(&project, &["identity", "set", "--status", "project-busy"]);
    let project_card = run_in_account(&project, &["identity", "show"]);
    assert!(
        project_card.contains("  name:       Project profile\n"),
        "{project_card}"
    );
    assert!(project_card.contains("  status:     project-busy\n"));
    assert_eq!(run_in_account(&project, &["whois"]), project_card);
    assert_eq!(
        run_in_account(&machine, &["identity", "show"]),
        machine_card
    );

    run_in_account(&project, &["msg", "profile-scope-proof"]);
    let inbox: serde_json::Value = serde_json::from_str(&run_in_account(
        &project,
        &["inbox", "--json", "--limit", "32"],
    ))
    .unwrap();
    let events: Vec<airc_core::TranscriptEvent> =
        serde_json::from_value(inbox["events"].clone()).unwrap();
    let messages: Vec<_> = events
        .iter()
        .filter(|event| {
            event.body.as_ref().and_then(airc_core::Body::as_text) == Some("profile-scope-proof")
        })
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].peer_id.as_uuid(), project_peer);
    assert!(
        events.iter().any(|event| {
            let Some(airc_core::Body::Json(body)) = &event.body else {
                return false;
            };
            let Ok(airc_core::identity::IdentityEvent::PeerIdentityCard(card)) =
                serde_json::from_value(body.clone())
            else {
                return false;
            };
            event.peer_id.as_uuid() == project_peer
                && card.peer_id.as_uuid() == project_peer
                && card.identity.name == "Project profile"
                && card.identity.status == "project-busy"
        }),
        "the project sender must publish its own updated card"
    );
    for (scope, before, key) in [
        (&machine, &machine_init, &machine_key),
        (&project, &project_init, &project_key),
    ] {
        let after = run_in_account(scope, &["init"]);
        for field in ["peer_id:", "client_id:", "peer_spec:"] {
            assert_eq!(
                after.lines().find(|line| line.starts_with(field)),
                before.lines().find(|line| line.starts_with(field)),
                "profile edits must preserve {field}",
            );
        }
        assert_eq!(&std::fs::read(scope.join("identity.key")).unwrap(), key);
    }
}

#[test]
fn identity_import_continuum_merges_into_store_without_clearing_status() {
    let workspace = common::daemon_tempdir();
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
    let workspace = common::daemon_tempdir();
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
    let workspace = common::daemon_tempdir();
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
    run_ok_as(home, airc_store::DEFAULT_AGENT_NAME, args, stdin)
}

fn run_ok_as(home: &std::path::Path, agent_name: &str, args: &[&str], stdin: &str) -> String {
    run_ok_as_in_account(home, agent_name, None, args, stdin)
}

fn run_ok_as_in_account(
    home: &std::path::Path,
    agent_name: &str,
    account: Option<&std::path::Path>,
    args: &[&str],
    stdin: &str,
) -> String {
    use std::io::Write;
    let mut command = Command::new(airc_core());
    command
        .arg("--home")
        .arg(home)
        .args(args)
        .env("AIRC_AGENT_NAME", agent_name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(account) = account {
        command.env("HOME", account).env("USERPROFILE", account);
    }
    let mut child = command.spawn().expect("airc-core must spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("airc-core must exit");
    common::assert_success(
        &output,
        &format!("airc-core {args:?} as {agent_name} in {}", home.display()),
    );
    String::from_utf8(output.stdout).unwrap()
}
