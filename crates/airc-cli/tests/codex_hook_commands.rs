//! End-to-end coverage for `airc-core codex-hook ...`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc-core")
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
fn codex_hook_installer_replaces_existing_managed_hook() {
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
    let managed = managed_hook_commands(&commands);
    assert_eq!(
        managed.len(),
        1,
        "expected one managed hook, got {commands:?}"
    );
    assert_source_command(&managed[0]);
}

#[test]
fn codex_hook_installer_replaces_existing_managed_hook_command() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(codex_home.join("config.toml"), "[features]\nhooks = true\n")
        .expect("write config");
    std::fs::write(
        codex_home.join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"airc codex-hook user-prompt-submit"}]}]}}"#,
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

    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(codex_home.join("hooks.json")).unwrap())
            .expect("hooks json");
    let commands = hook_commands(&hooks);
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.ends_with("codex-hook user-prompt-submit"))
            .count(),
        1,
        "install must replace existing managed hook commands, got {commands:?}"
    );
    assert_eq!(commands.len(), 1, "expected one command, got {commands:?}");
    assert_source_command(&commands[0]);
}

#[test]
fn codex_hook_installer_removes_managed_filesystem_profile() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        r#"
[features]
codex_hooks = true

# airc filesystem permissions
[permissions.airc.filesystem]
enabled = true

[permissions.airc.filesystem.write]
paths = ["/tmp"]

[permissions.airc.network]
enabled = true
"#,
    )
    .expect("write config");

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
    assert!(!config.contains("permissions.airc.filesystem"));
    assert!(!config.contains("paths = [\"/tmp\"]"));
    assert!(config.contains("[permissions.airc.network]"));
    assert!(config.contains("hooks = true"));
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
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo existing"},{"type":"command","command":"airc-core codex-hook user-prompt-submit"}]}]}}"#,
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
    assert!(!commands.contains(&"airc-core codex-hook user-prompt-submit".to_string()));
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let output = command_for_home(home)
        .args(["--home"])
        .arg(home)
        .args(args)
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

fn run_hook(home: &Path, args: &[&str], stdin: &str) -> String {
    let mut child = command_for_home(home)
        .args(["--home"])
        .arg(home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc-core hook must spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("write hook stdin");
    let output = child.wait_with_output().expect("wait for hook");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn command_for_home(home: &Path) -> Command {
    let mut command = Command::new(airc_core());
    let account_home = home.parent().unwrap_or(home);
    command.env("HOME", account_home);
    command.env("USERPROFILE", account_home);
    command
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

fn managed_hook_commands(commands: &[String]) -> Vec<String> {
    commands
        .iter()
        .filter(|command| command.ends_with("codex-hook user-prompt-submit"))
        .cloned()
        .collect()
}

fn assert_source_command(command: &str) {
    assert!(
        command.ends_with(" codex-hook user-prompt-submit"),
        "managed hook must call the Codex hook subcommand, got {command:?}"
    );
    assert!(
        command.contains(source_airc_command_fragment()),
        "managed hook must use the source-installed airc command, got {command:?}"
    );
    assert!(
        !command.starts_with("airc "),
        "managed hook must not depend on PATH, got {command:?}"
    );
    assert!(
        !command.starts_with("airc-core "),
        "managed hook must not call airc-core directly, got {command:?}"
    );
}

#[cfg(windows)]
fn source_airc_command_fragment() -> &'static str {
    "\\airc.cmd\" codex-hook user-prompt-submit"
}

#[cfg(not(windows))]
fn source_airc_command_fragment() -> &'static str {
    "/airc' codex-hook user-prompt-submit"
}
