//! End-to-end coverage for `airc-rs codex-hook ...`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

#[test]
fn codex_hook_emits_context_and_advances_cursor() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");
    let cursor = workspace.path().join("cursor.json");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "first unread"]);
    run_ok(&home, &["send", "second unread"]);

    let output = run_hook(
        &home,
        &[
            "codex-hook",
            "user-prompt-submit",
            "--cursor-file",
            cursor.to_str().unwrap(),
            "--include-self",
        ],
        "{}",
    );
    let context = additional_context(&output);
    assert!(context.contains("AIRC: 2 unread"));
    assert!(context.contains("first unread"));
    assert!(context.contains("second unread"));
    assert!(cursor.exists(), "hook must persist its transcript cursor");

    let second = run_hook(
        &home,
        &[
            "codex-hook",
            "user-prompt-submit",
            "--cursor-file",
            cursor.to_str().unwrap(),
            "--include-self",
        ],
        "{}",
    );
    assert_eq!(
        second, "",
        "second hook call should be silent after cursor advance"
    );
}

#[test]
fn codex_hook_excludes_self_echoes_but_still_advances_cursor() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");
    let cursor = workspace.path().join("cursor.json");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "own message"]);

    let output = run_hook(
        &home,
        &[
            "codex-hook",
            "user-prompt-submit",
            "--cursor-file",
            cursor.to_str().unwrap(),
        ],
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    assert_eq!(output, "");
    assert!(cursor.exists(), "self echoes should not replay forever");
}

#[test]
fn codex_hook_raw_mode_preserves_full_event_lines() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "raw line visible"]);

    let output = run_hook(
        &home,
        &[
            "codex-hook",
            "user-prompt-submit",
            "--raw",
            "--include-self",
        ],
        "{}",
    );
    let context = additional_context(&output);
    assert!(context.contains("raw line visible"));
    assert!(context.contains('['));
    assert!(context.contains(']'));
}

#[test]
fn codex_hook_installer_replaces_legacy_python_hook() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        "[features]\ncodex_hooks = true\nother = true\n",
    )
    .expect("write config");
    std::fs::write(
        codex_home.join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo existing"},{"type":"command","command":"airc codex-hook user-prompt-submit"}]}]}}"#,
    )
    .expect("write hooks");

    run_ok(
        &home,
        &[
            "codex-hook",
            "install-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config");
    assert!(config.contains("hooks = true"));
    assert!(config.contains("other = true"));
    assert!(!config.contains("codex_hooks"));

    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(codex_home.join("hooks.json")).unwrap())
            .expect("hooks json");
    let commands = hook_commands(&hooks);
    assert!(commands.contains(&"echo existing".to_string()));
    assert!(commands.contains(&"airc-rs codex-hook user-prompt-submit".to_string()));
    assert!(!commands.contains(&"airc codex-hook user-prompt-submit".to_string()));
}

#[test]
fn codex_hook_uninstaller_removes_managed_hooks_only() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        "[features]\nhooks = true\nother = true\n",
    )
    .expect("write config");
    std::fs::write(
        codex_home.join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo existing"},{"type":"command","command":"airc-rs codex-hook user-prompt-submit"}]}]}}"#,
    )
    .expect("write hooks");

    run_ok(
        &home,
        &[
            "codex-hook",
            "uninstall-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config");
    assert!(!config.contains("hooks = true"));
    assert!(config.contains("other = true"));

    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(codex_home.join("hooks.json")).unwrap())
            .expect("hooks json");
    let commands = hook_commands(&hooks);
    assert!(commands.contains(&"echo existing".to_string()));
    assert!(!commands.contains(&"airc-rs codex-hook user-prompt-submit".to_string()));
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

fn run_hook(home: &Path, args: &[&str], stdin: &str) -> String {
    let mut child = Command::new(airc_rs())
        .arg("--home")
        .arg(home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc-rs hook must spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("write hook stdin");
    let output = child.wait_with_output().expect("wait for hook");
    assert!(
        output.status.success(),
        "airc-rs {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn additional_context(output: &str) -> String {
    let value: Value = serde_json::from_str(output).expect("hook JSON");
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext string")
        .to_string()
}

fn hook_commands(hooks: &Value) -> Vec<String> {
    hooks["hooks"]["UserPromptSubmit"]
        .as_array()
        .expect("UserPromptSubmit array")
        .iter()
        .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
        .filter_map(|hook| hook["command"].as_str().map(ToString::to_string))
        .collect()
}
