//! End-to-end coverage for `airc codex-hook ...`.

use std::process::Stdio;
use std::thread;
use std::time::Duration;

use serde_json::Value;
mod common;
mod hook_support;
use hook_support::*;

#[test]
fn configure_installer_merges_config_and_rotates_token_without_duplicate_tables() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let path = codex_home.join("config.toml");
    std::fs::write(
        &path,
        "[shell_environment_policy.set]\nKEEP = 'yes'\n[rules]\nkeep = true\n",
    )
    .unwrap();
    let args = [
        "codex-hook",
        "configure-installer",
        "--codex-home",
        codex_home.to_str().unwrap(),
        "--token-stdin",
        "--command-rules",
    ];
    for token in ["synthetic-first", "synthetic-second", "synthetic-second"] {
        let output = run_hook(&home, &args, token);
        assert!(!output.contains(token));
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            doc["shell_environment_policy"]["set"]["GH_TOKEN"].as_str(),
            Some(token)
        );
        assert_eq!(
            doc["shell_environment_policy"]["set"]["KEEP"].as_str(),
            Some("yes")
        );
        assert_eq!(doc["rules"]["keep"].as_bool(), Some(true));
    }
}

#[test]
fn configure_installer_refuses_invalid_config_without_writing_or_echoing_it() {
    let workspace = common::daemon_tempdir();
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let path = codex_home.join("config.toml");
    let original = "secret = 'PRIVATE-SYNTHETIC'\nsecret = 'PRIVATE-SYNTHETIC'\n";
    std::fs::write(&path, original).unwrap();
    let output = command_for_home(&workspace.path().join("agent"))
        .args(["codex-hook", "configure-installer", "--codex-home"])
        .arg(&codex_home)
        .arg("--command-rules")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE-SYNTHETIC"));
}

#[test]
fn configure_installer_uninstall_removes_owned_values_but_preserves_siblings() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let path = codex_home.join("config.toml");
    std::fs::write(
        &path,
        "shell_environment_policy = { set = { KEEP = 'yes' } }\n",
    )
    .unwrap();
    run_hook(
        &home,
        &[
            "codex-hook",
            "configure-installer",
            "--codex-home",
            codex_home.to_str().unwrap(),
            "--token-stdin",
            "--command-rules",
        ],
        "synthetic-token",
    );
    // A user can add settings after install; uninstall must remove only owned keys.
    let mut doc = std::fs::read_to_string(&path)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    doc["rules"]["keep"] = toml_edit::value(true);
    std::fs::write(&path, doc.to_string()).unwrap();
    run_ok(
        &home,
        &[
            "codex-hook",
            "uninstall-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );
    let doc = std::fs::read_to_string(&path)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    assert!(doc["shell_environment_policy"]["set"]
        .get("GH_TOKEN")
        .is_none());
    assert_eq!(
        doc["shell_environment_policy"]["set"]["KEEP"].as_str(),
        Some("yes")
    );
    assert!(doc["rules"].get("prefix_rules").is_none());
    assert_eq!(doc["rules"]["keep"].as_bool(), Some(true));

    // With no user siblings the empty owned table must disappear, so reinstall works.
    std::fs::write(&path, "").unwrap();
    for _ in 0..2 {
        run_ok(
            &home,
            &[
                "codex-hook",
                "configure-installer",
                "--codex-home",
                codex_home.to_str().unwrap(),
                "--command-rules",
            ],
        );
        let doc = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(doc["rules"].get("prefix_rules").is_some());
        run_ok(
            &home,
            &[
                "codex-hook",
                "uninstall-hooks",
                "--codex-home",
                codex_home.to_str().unwrap(),
            ],
        );
        let doc = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(!doc.contains_key("rules"));
    }
}

#[test]
fn codex_hook_emits_context_and_advances_cursor() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
    run_ok(&home, &["send", "first unread"]);
    run_ok(&home, &["send", "second unread"]);

    let output = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit", "--include-self"],
        "{}",
    );
    let context = additional_context(&output);
    assert!(context.contains("AIRC: 2 unread"));
    assert!(context.contains("first unread"));
    assert!(context.contains("second unread"));

    let second = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit", "--include-self"],
        "{}",
    );
    assert_eq!(
        second, "",
        "second hook call should be silent after cursor advance"
    );
}

#[test]
fn codex_hook_excludes_self_echoes_but_still_advances_cursor() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "own message"]);

    let output = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit"],
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    assert_eq!(output, "");
    let second = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit"],
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    assert_eq!(second, "", "self echoes should not replay forever");
}

#[test]
fn oversized_hook_reference_recovers_whole_body_after_acknowledgement() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
    let body = "recovery-界".repeat(700);
    run_ok(&home, &["send", &body]);
    run_ok(&home, &["send", "fresh-review-request"]);
    let args = &["codex-hook", "user-prompt-submit", "--include-self"];
    let context = additional_context(&run_hook(&home, args, "{}"));
    assert!(context.contains("body retained:"));
    assert!(context.contains("fresh-review-request"));
    assert!(!context.contains(&body));
    // Execute the exact public recovery command emitted by the daemon-backed
    // hook, rather than assuming embedded event lamports share its cursor domain.
    let recovery = context
        .split('`')
        .find(|part| part.starts_with("airc inbox "))
        .expect("recoverable reference");
    let recovery_args: Vec<_> = recovery.split_whitespace().skip(1).collect();
    assert!(run_hook(&home, args, "{}").trim().is_empty());
    let recovered = run_ok(&home, &recovery_args);
    let json: Value = serde_json::from_str(&recovered).unwrap();
    assert!(
        json.to_string().contains(&body),
        "full body remains in inbox"
    );
}

#[test]
fn codex_hook_filters_by_runtime_client_header_not_persisted_identity() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, Some("codex:thread-1"), "codex-hook");
    run_ok_with_client(&home, "codex:thread-1", &["send", "own runtime"]);
    run_ok_with_client(&home, "claude:session-1", &["send", "peer runtime"]);

    let output = run_hook_with_client(
        &home,
        "codex:thread-1",
        &["codex-hook", "user-prompt-submit"],
        "{}",
    );
    let context = additional_context(&output);
    assert!(!context.contains("own runtime"));
    assert!(context.contains("peer runtime"));
}

#[test]
fn codex_hook_keeps_stamped_peer_events_when_runtime_client_is_unknown() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
    run_ok_with_client(
        &home,
        "claude:session-1",
        &["send", "peer despite shared home"],
    );

    let output = run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");
    let context = additional_context(&output);
    assert!(context.contains("peer despite shared home"));
}

#[test]
fn codex_hook_suggests_claimable_work_on_work_events() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
    run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "wire claimable work into agent feed",
            "--priority",
            "p0",
        ],
    );

    let output = run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");
    let context = additional_context(&output);

    assert!(context.contains("AIRC work: 1 claimable P0/P1"));
    assert!(context.contains("wire claimable work into agent feed"));
    assert!(context.contains("airc work claim"));
}

#[test]
fn codex_hook_keeps_presence_quiet_and_reports_availability_with_work_state() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
    run_ok(
        &home,
        &[
            "work",
            "availability",
            "--repo",
            "CambrianTech/airc",
            "--state",
            "ready",
            "--note",
            "available for queue work",
            "--ttl-ms",
            "60000",
        ],
    );

    let output = run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");
    assert!(
        output.trim().is_empty(),
        "presence alone must not interrupt: {output}"
    );
    run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "ordinary queued work",
            "--priority",
            "p2",
        ],
    );
    let output = run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");
    let context = additional_context(&output);

    assert!(context.contains("AIRC work: 0 claimable P0/P1"));
    assert!(context.contains("availability ready=1 busy=0 away=0 stale=0"));
    assert!(context.contains("publish ready/busy/away"));
}

#[test]
fn codex_hook_raw_mode_preserves_full_event_lines() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
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
fn codex_hook_poll_prints_plain_digest_and_advances_cursor() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
    run_ok(&home, &["send", "poll first unread"]);
    run_ok(&home, &["send", "poll second unread"]);

    let output = run_ok(
        &home,
        &["codex-hook", "poll", "--include-self", "--max-items", "4"],
    );
    assert!(output.contains("AIRC: 2 unread"));
    assert!(output.contains("poll first unread"));
    assert!(output.contains("poll second unread"));
    assert!(
        serde_json::from_str::<Value>(&output).is_err(),
        "poll is a plain CLI feed, not hook JSON"
    );

    let second = run_ok(&home, &["codex-hook", "poll", "--include-self"]);
    assert_eq!(second, "", "poll should share the hook cursor");
}

// Regression: the CLI checkpoints only the emitted prefix, shared by poll and hooks.
#[test]
fn digest_continuation_replays_remaining_messages_across_hook_surfaces() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    attach(&home, Some("codex:bounded"), "codex-hook");
    for text in [
        "first instruction",
        "second instruction",
        "third instruction",
    ] {
        run_ok(&home, &["send", text]);
    }
    let first = run_ok_with_client(
        &home,
        "codex:bounded",
        &["codex-hook", "poll", "--include-self", "--max-items", "1"],
    );
    assert!(first.contains("first instruction"));
    assert!(!first.contains("second instruction"));
    assert!(first.contains("More unread messages remain"));
    let second = run_hook_with_client(
        &home,
        "codex:bounded",
        &[
            "codex-hook",
            "user-prompt-submit",
            "--include-self",
            "--max-items",
            "1",
        ],
        "{}",
    );
    let second = additional_context(&second);
    assert!(second.contains("second instruction"));
    assert!(!second.contains("third instruction"));
    let third = run_ok_with_client(
        &home,
        "codex:bounded",
        &["codex-hook", "poll", "--include-self", "--max-items", "1"],
    );
    assert!(third.contains("third instruction"));
    assert!(!third.contains("first instruction"));
}

#[test]
fn codex_hook_poll_filters_runtime_self_echoes() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, Some("codex:thread-1"), "codex-hook");
    run_ok_with_client(&home, "codex:thread-1", &["send", "poll own runtime"]);
    run_ok_with_client(&home, "claude:session-1", &["send", "poll peer runtime"]);

    let output = run_ok_with_client(
        &home,
        "codex:thread-1",
        &["codex-hook", "poll", "--max-items", "4"],
    );
    assert!(!output.contains("poll own runtime"));
    assert!(output.contains("poll peer runtime"));

    let second = run_ok_with_client(&home, "codex:thread-1", &["codex-hook", "poll"]);
    assert_eq!(
        second, "",
        "self-filtered events should still advance the cursor"
    );
}

#[test]
fn codex_hook_poll_waits_for_one_new_event() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");

    let child = command_for_home(&home)
        .args(["--home"])
        .arg(&home)
        .args(["codex-hook", "poll", "--include-self", "--wait-ms", "2000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc poll must spawn");

    thread::sleep(Duration::from_millis(150));
    run_ok(&home, &["send", "delayed poll event"]);

    let output = child.wait_with_output().expect("wait for poll");
    common::assert_success(&output, "poll");
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    assert!(stdout.contains("delayed poll event"));
}

#[test]
fn codex_hook_installer_replaces_existing_managed_hook() {
    let workspace = common::daemon_tempdir();
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
    let workspace = common::daemon_tempdir();
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
fn codex_hook_installer_adds_turn_contract_when_unset() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(codex_home.join("config.toml"), "[features]\n").expect("write config");

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
    assert!(config.contains("AIRC-CODEX-INSTRUCTIONS-START"));
    assert!(config.contains("developer_instructions"));
    assert!(config.contains("UserPromptSubmit and PostToolUse"));
    assert!(config.contains("Do not add routine manual polling"));
    assert!(config.contains("hooks = true"));
}

#[test]
fn codex_hook_installer_preserves_custom_developer_instructions() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        "developer_instructions = \"custom contract\"\n",
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
    assert!(config.contains("developer_instructions = \"custom contract\""));
    assert!(!config.contains("AIRC-CODEX-INSTRUCTIONS-START"));
    assert!(!config.contains("airc codex-hook poll --wait-ms 1000"));
    assert!(config.contains("hooks = true"));
}

#[test]
fn codex_hook_installer_removes_managed_filesystem_profile() {
    let workspace = common::daemon_tempdir();
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
    let workspace = common::daemon_tempdir();
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

#[test]
fn saved_resume_brief_delivers_at_prompt_boundary_but_not_during_tools() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    run_ok(
        &home,
        &[
            "agent-resume",
            "set",
            "--brief",
            "Finish current owner task",
            "--manual",
            "AGENTS.md",
        ],
    );
    let active = run_hook_with_client(
        &home,
        "codex:resume-a",
        &["codex-hook", "post-tool-use"],
        "{}",
    );
    assert!(!active.contains("Finish current owner task"));
    let first = run_hook_with_client(
        &home,
        "codex:resume-a",
        &["codex-hook", "user-prompt-submit"],
        "{}",
    );
    let context = additional_context(&first);
    assert!(context.contains("Finish current owner task"));
    assert!(context.contains("Manual/skill: AGENTS.md"));
    let second = run_hook_with_client(
        &home,
        "codex:resume-a",
        &["codex-hook", "user-prompt-submit"],
        "{}",
    );
    assert!(!second.contains("Finish current owner task"));
    let independent = run_hook_with_client(
        &home,
        "codex:resume-b",
        &["codex-hook", "user-prompt-submit"],
        "{}",
    );
    assert!(additional_context(&independent).contains("Finish current owner task"));
    run_ok(&home, &["agent-resume", "clear"]);
    let disabled = run_hook_with_client(
        &home,
        "codex:resume-c",
        &["codex-hook", "user-prompt-submit"],
        "{}",
    );
    assert!(!disabled.contains("Finish current owner task"));
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
    // Post-demolition contract (PR D): the public command IS the
    // Rust binary on PATH at `airc`, not a source-tree wrapper
    // script. The managed hook command resolves to PATH `airc`,
    // exactly the command a stranger reading the README would type.
    assert_eq!(
        command, "airc codex-hook user-prompt-submit",
        "managed hook must be the PATH `airc` command, got {command:?}"
    );
    assert!(
        !command.starts_with("airc-core "),
        "managed hook must not call legacy airc-core suffix, got {command:?}"
    );
}
