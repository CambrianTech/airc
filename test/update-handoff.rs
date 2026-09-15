// Run with rustc --edition 2021 --test test/update-handoff.rs -o <temp>/handoff,
// then <temp>/handoff --nocapture. Only tiny fixture programs are compiled.
#[path = "../crates/airc-cli/src/update_artifact.rs"]
mod update_artifact;
#[path = "../crates/airc-cli/src/update_shutdown.rs"]
mod update_shutdown;

use std::path::Path;

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn public_handoff_builds_once_before_stop_and_rejects_bad_preparations() {
    let root = std::env::temp_dir().join(format!("airc-handoff-proof-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let result = std::panic::catch_unwind(|| {
        let source = root.join("source with spaces");
        let tools = root.join("tools");
        let target = root.join("target");
        let home = root.join("home");
        for dir in [
            &source,
            &tools,
            &home,
            &target.join("release"),
            &source.join(".git"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        write(&source.join("install.sh"), include_str!("../install.sh"));
        write(
            &tools.join("git"),
            "#!/bin/sh\necho abcdef1234567890abcdef1234567890abcdef1234\n",
        );
        write(&tools.join("uname"), "#!/bin/sh\necho Linux\n");
        write(
            &tools.join("cargo"),
            r#"#!/bin/sh
[ ! -f "$AIRC_FIXTURE_ROOT/stopped" ] || { echo 'CARGO DURING OUTAGE' >&2; exit 88; }
case "$1" in
  --version) echo 'cargo 1.95.0' ;;
  metadata) printf '{"target_directory":"%s/target"}\n' "$AIRC_FIXTURE_ROOT" ;;
  build)
    echo build >> "$AIRC_FIXTURE_ROOT/events"
    [ ! -f "$AIRC_FIXTURE_ROOT/fail-build" ] || exit 42
    ;;
  *) exit 90 ;;
esac
"#,
        );
        let good = "#!/bin/sh\ncase \"$1\" in version) echo 'build: abcdef1234567890' ;; --version) echo 'airc 0.1.0' ;; esac\n";
        write(&target.join("release/airc"), good);
        let bash = std::env::var_os("AIRC_TEST_BASH").unwrap_or_else(|| "bash".into());
        let old_path = std::env::var_os("PATH").unwrap();
        let mut paths = vec![tools.clone()];
        paths.extend(std::env::split_paths(&old_path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        std::env::set_var("HOME", &home);
        std::env::set_var("BIN_DIR", home.join("bin"));
        std::env::set_var(
            "AIRC_FIXTURE_ROOT",
            root.to_string_lossy().replace('\\', "/"),
        );
        // Git Bash initializes its own system PATH when launched from Windows.
        // Install fixture tools after that initialization, in Bash path syntax.
        write(&root.join("bash-env"), "fixture_root=\"$(cd \"$AIRC_FIXTURE_ROOT\" && pwd)\"\nexport PATH=\"$fixture_root/tools:$PATH\"\n");
        std::env::set_var("BASH_ENV", root.join("bash-env"));
        for key in [
            "AIRC_SKIP_PREREQS",
            "AIRC_SKIP_GIT_HOOKS",
            "AIRC_SKIP_CODEX_CONFIG",
            "AIRC_SKIP_CODEX_INSTRUCTIONS",
            "AIRC_SKIP_CODEX_HOOKS",
            "AIRC_SKIP_CODEX_TOKEN",
            "AIRC_SKIP_CODEX_RULES",
            "AIRC_INSTALL_YES",
        ] {
            std::env::set_var(key, "1");
        }
        std::env::remove_var("AIRC_SKIP_RUST_BUILD");
        let prepared =
            update_artifact::PreparedInstall::prepare(&bash, &source, "abcdef1234567890").unwrap();
        // Prove install uses its owned snapshot, even if a different build has
        // since replaced the shared Cargo output.
        write(
            &target.join("release/airc"),
            "#!/bin/sh\necho 'build: deadbee'\n",
        );
        prepared
            .install_after(|| {
                std::fs::write(root.join("stopped"), "daemon maintenance")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("events")).unwrap(),
            "build\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("bin/airc")).unwrap(),
            good
        );
        drop(prepared);
        std::fs::remove_file(root.join("stopped")).unwrap();
        // A stale artifact and a failed compiler both abort preparation while
        // the daemon is still up; install_after cannot be reached.
        assert!(
            update_artifact::PreparedInstall::prepare(&bash, &source, "abcdef1234567890").is_err()
        );
        assert!(!root.join("stopped").exists());
        write(&root.join("fail-build"), "fail");
        assert!(
            update_artifact::PreparedInstall::prepare(&bash, &source, "abcdef1234567890").is_err()
        );
        assert!(!root.join("stopped").exists());
        // Wrong expected source SHA is rejected before any build.
        let before = std::fs::read_to_string(root.join("events")).unwrap();
        assert!(update_artifact::PreparedInstall::prepare(&bash, &source, "deadbee").is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("events")).unwrap(),
            before
        );
    });
    std::fs::remove_dir_all(&root).unwrap();
    result.unwrap();
}

#[cfg(windows)]
#[test]
#[ignore]
fn delayed_shutdown_fixture() {
    std::thread::sleep(std::time::Duration::from_millis(400));
}

#[cfg(windows)]
#[test]
fn shutdown_wait_observes_process_exit_not_stop_acknowledgement() {
    use std::os::windows::process::CommandExt;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "delayed_shutdown_fixture", "--ignored"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW, fixture only
        .spawn()
        .unwrap();
    let pid_file = std::env::temp_dir().join(format!("airc-exit-proof-{}.pid", child.id()));
    std::fs::write(&pid_file, child.id().to_string()).unwrap();
    let process = update_shutdown::DaemonExit::capture(&pid_file).unwrap();
    std::fs::remove_file(&pid_file).unwrap(); // stop acknowledged, pidfile gone
    assert_eq!(
        process
            .wait(std::time::Duration::from_millis(1))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
    process.wait(std::time::Duration::from_secs(5)).unwrap();
    assert!(child.wait().unwrap().success());
}
