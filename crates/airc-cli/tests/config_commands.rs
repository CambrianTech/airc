//! End-to-end coverage for `airc-rs config ...`.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

#[test]
fn config_read_channels_preserves_order() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(
        home.join("config.json"),
        r#"{"subscribed_channels":["general","airc","continuum"]}"#,
    )
    .unwrap();

    let output = run_ok(home, &["config", "read-channels"]);

    assert_eq!(output, "general\nairc\ncontinuum\n");
}

#[test]
fn config_read_channels_missing_config_is_empty() {
    let workspace = TempDir::new().expect("tempdir");

    let output = run_ok(workspace.path(), &["config", "read-channels"]);

    assert!(output.is_empty());
}

#[test]
fn config_default_channel_prints_first_channel() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(
        home.join("config.json"),
        r#"{"subscribed_channels":["general","airc"]}"#,
    )
    .unwrap();

    let output = run_ok(home, &["config", "default-channel"]);

    assert_eq!(output, "general\n");
}

#[test]
fn config_get_channel_gist_prints_mapping() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(
        home.join("config.json"),
        r#"{"channel_gists":{"general":"gist-general","airc":"gist-airc"}}"#,
    )
    .unwrap();

    let output = run_ok(home, &["config", "get-channel-gist", "--channel", "airc"]);

    assert_eq!(output, "gist-airc\n");
}

#[test]
fn config_list_channel_gists_prints_tab_separated_mappings() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(
        home.join("config.json"),
        r#"{"channel_gists":{"airc":"gist-airc","general":"gist-general"}}"#,
    )
    .unwrap();

    let output = run_ok(home, &["config", "list-channel-gists"]);

    assert_eq!(output, "airc\tgist-airc\ngeneral\tgist-general\n");
}

#[test]
fn config_subscribe_unsubscribe_round_trips() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(
        home.join("config.json"),
        r#"{"subscribed_channels":["general"]}"#,
    )
    .unwrap();

    run_ok(home, &["config", "subscribe", "--channel", "airc"]);
    run_ok(home, &["config", "subscribe", "--channel", "airc"]);
    assert_eq!(
        run_ok(home, &["config", "read-channels"]),
        "general\nairc\n"
    );

    run_ok(home, &["config", "unsubscribe", "--channel", "general"]);
    assert_eq!(run_ok(home, &["config", "read-channels"]), "airc\n");
}

#[test]
fn config_subscribe_first_promotes_default_channel() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(
        home.join("config.json"),
        r#"{"subscribed_channels":["general","airc"]}"#,
    )
    .unwrap();

    run_ok(
        home,
        &["config", "subscribe", "--channel", "airc", "--first"],
    );

    assert_eq!(
        run_ok(home, &["config", "read-channels"]),
        "airc\ngeneral\n"
    );
}

#[test]
fn config_set_channel_gist_sets_and_clears_mapping() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();

    run_ok(
        home,
        &[
            "config",
            "set-channel-gist",
            "--channel",
            "general",
            "--gist-id",
            "gist-general",
        ],
    );
    assert_eq!(
        run_ok(
            home,
            &["config", "get-channel-gist", "--channel", "general"]
        ),
        "gist-general\n"
    );

    run_ok(
        home,
        &["config", "set-channel-gist", "--channel", "general"],
    );
    assert_eq!(
        run_ok(
            home,
            &["config", "get-channel-gist", "--channel", "general"]
        ),
        ""
    );
}

#[test]
fn config_get_set_unset_round_trips() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(
        home.join("config.json"),
        r#"{"name":"alice","host":"localhost","identity":{"role":"agent"}}"#,
    )
    .unwrap();

    assert_eq!(run_ok(home, &["config", "get-name"]), "alice\n");
    assert_eq!(
        run_ok(home, &["config", "get", "identity", "{}"]),
        "{\"role\":\"agent\"}\n"
    );

    run_ok(home, &["config", "set", "--key", "name", "--value", "bob"]);
    assert_eq!(run_ok(home, &["config", "get-name"]), "bob\n");

    run_ok(home, &["config", "unset-keys", "host"]);
    assert_eq!(
        run_ok(home, &["config", "get", "host", "missing"]),
        "missing\n"
    );
}

#[test]
fn config_set_name_matches_legacy_command() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();

    run_ok(home, &["config", "set-name", "--name", "codex-tab"]);

    assert_eq!(run_ok(home, &["config", "get-name"]), "codex-tab\n");
}

#[test]
fn config_parted_rooms_are_idempotent() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();

    run_ok(home, &["config", "record-parted", "--room", "general"]);
    run_ok(home, &["config", "record-parted", "--room", "general"]);
    run_ok(home, &["config", "record-parted", "--room", "airc"]);
    assert_eq!(run_ok(home, &["config", "read-parted"]), "general\nairc\n");

    run_ok(home, &["config", "clear-parted", "--room", "general"]);
    assert_eq!(run_ok(home, &["config", "read-parted"]), "airc\n");
}

#[test]
fn config_set_host_block_writes_handshake_fields() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path();
    fs::write(home.join("config.json"), r#"{"name":"joiner"}"#).unwrap();

    run_ok(
        home,
        &[
            "config",
            "set-host-block",
            "--host-airc-home",
            "/tmp/host airc",
            "--host-name",
            "host",
            "--host-port",
            "7551",
            "--host-ssh-pub",
            "ssh-ed25519 AAA host",
            "--host-identity-json",
            r#"{"role":"host"}"#,
        ],
    );

    assert_eq!(
        run_ok(home, &["config", "get", "host_airc_home"]),
        "/tmp/host airc\n"
    );
    assert_eq!(run_ok(home, &["config", "get", "host_name"]), "host\n");
    assert_eq!(run_ok(home, &["config", "get", "host_port"]), "7551\n");
    assert_eq!(
        run_ok(home, &["config", "get", "host_identity", "{}"]),
        "{\"role\":\"host\"}\n"
    );
    assert_eq!(run_ok(home, &["config", "get-name"]), "joiner\n");
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
