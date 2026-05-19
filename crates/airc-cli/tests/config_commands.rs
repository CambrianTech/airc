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
