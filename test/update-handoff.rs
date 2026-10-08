// Run with rustc --edition 2021 --test test/update-handoff.rs -o <temp>/handoff,
// then <temp>/handoff --nocapture. Only tiny fixture programs are compiled.
extern crate self as airc_core;
#[path = "../crates/airc-core/src/process.rs"]
mod process;
#[path = "../crates/airc-cli/src/update_shutdown/process_exit.rs"]
mod process_exit;
#[path = "../crates/airc-cli/src/update_artifact.rs"]
mod update_artifact;

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
  metadata)
    if [ -f "$AIRC_FIXTURE_ROOT/metadata-case" ]; then
      case "$(cat "$AIRC_FIXTURE_ROOT/metadata-case")" in
        failure) exit 42 ;;
        malformed) echo 'not-json'; exit 0 ;;
        missing) echo '{}'; exit 0 ;;
        empty) echo '{"target_directory":""}'; exit 0 ;;
      esac
    fi
    printf '{"target_directory":"%s/target"}\n' "$AIRC_FIXTURE_ROOT"
    ;;
  build)
    echo build >> "$AIRC_FIXTURE_ROOT/events"
    [ ! -f "$AIRC_FIXTURE_ROOT/fail-build" ] || exit 42
    ;;
  *) exit 90 ;;
esac
"#,
        );
        let good = r#"#!/bin/sh
selected_scope=0
if [ "$1" = '--home' ]; then
  [ "$2" = "${AIRC_HOME:-$HOME/.airc}" ] || exit 93
  selected_scope=1
  shift 2
fi
case "$1" in
  version) echo 'build: abcdef1234567890' ;;
  --version) echo 'airc 0.1.0' ;;
  codex-hook) echo hook >> "$AIRC_FIXTURE_ROOT/events" ;;
  update)
    [ "$selected_scope" = 1 ] || exit 94
    [ "$2" = '--adopt-installed' ] || exit 92
    echo adopt >> "$AIRC_FIXTURE_ROOT/events"
    [ ! -f "$AIRC_FIXTURE_ROOT/fail-adopt" ] || exit 73
    ;;
  *) exit 95 ;;
esac
"#;
        write(&target.join("release/airc"), good);
        let bash = std::env::var_os("AIRC_TEST_BASH").unwrap_or_else(|| "bash".into());
        #[cfg(windows)]
        {
            // PreparedInstall must select the native adapter even though its
            // legacy shell argument is deliberately unusable on Windows.
            std::env::set_var("AIRC_HANDOFF_BASH", &bash);
            write(
                &source.join("install.ps1"),
                r#"
param([string]$PrepareArtifact,[string]$PrebuiltArtifact,[string]$ExpectedBuild)
$mode=if($PrepareArtifact){'--prepare-artifact'}else{'--prebuilt'}
$artifact=if($PrepareArtifact){$PrepareArtifact}else{$PrebuiltArtifact}
$start=New-Object Diagnostics.ProcessStartInfo
$start.FileName=$env:AIRC_HANDOFF_BASH
$start.Arguments='--noprofile --norc "'+($PSScriptRoot -replace '\\','/')+'/install.sh" '+$mode+' "'+($artifact -replace '\\','/')+'" --expected-build "'+$ExpectedBuild+'"'
$start.UseShellExecute=$false;$start.CreateNoWindow=$true
$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
$child=[Diagnostics.Process]::Start($start)
try{$out=$child.StandardOutput.ReadToEndAsync();$err=$child.StandardError.ReadToEndAsync();$child.WaitForExit();[Console]::Out.Write($out.Result);[Console]::Error.Write($err.Result);exit $child.ExitCode}finally{$child.Dispose()}
"#,
            );
        }
        #[cfg(windows)]
        let bash = std::ffi::OsString::from("unusable-wsl-fixture.exe");
        let old_path = std::env::var_os("PATH").unwrap();
        let mut paths = vec![tools.clone()];
        paths.extend(std::env::split_paths(&old_path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        std::env::set_var("HOME", &home);
        std::env::set_var("BIN_DIR", home.join("wrong-dir"));
        std::env::set_var("BIN_TARGET", home.join("wrong-target"));
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
        // Regression: failed/malformed metadata must not guess source/target,
        // even when that directory contains a valid stale build.
        std::fs::create_dir_all(source.join("target/release")).unwrap();
        write(&source.join("target/release/airc"), good);
        for case in ["failure", "malformed", "missing", "empty"] {
            write(&root.join("metadata-case"), case);
            assert!(
                update_artifact::PreparedInstall::prepare(
                    &bash,
                    &source,
                    "abcdef1234567890",
                    &home.join("bin/airc")
                )
                .is_err(),
                "metadata case {case} used a guessed build directory"
            );
            assert!(!root.join("stopped").exists());
        }
        std::fs::remove_file(root.join("metadata-case")).unwrap();
        // The normal path must use Cargo's configured target, not the default.
        write(
            &source.join("target/release/airc"),
            "#!/bin/sh\necho 'build: deadbee'\n",
        );
        std::fs::remove_file(root.join("events")).unwrap();
        let prepared = update_artifact::PreparedInstall::prepare(
            &bash,
            &source,
            "abcdef1234567890",
            &home.join("bin/airc"),
        )
        .unwrap();
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
            "build\n",
            "prepare/prebuilt handoff must not adopt inside the updater's maintenance lease"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("bin/airc")).unwrap(),
            good
        );
        assert!(!home.join("wrong-dir").exists());
        assert!(!home.join("wrong-target").exists());
        drop(prepared);
        std::fs::remove_file(root.join("stopped")).unwrap();
        // A stale artifact and a failed compiler both abort preparation while
        // the daemon is still up; install_after cannot be reached.
        assert!(update_artifact::PreparedInstall::prepare(
            &bash,
            &source,
            "abcdef1234567890",
            &home.join("bin/airc")
        )
        .is_err());
        assert!(!root.join("stopped").exists());
        write(&root.join("fail-build"), "fail");
        assert!(update_artifact::PreparedInstall::prepare(
            &bash,
            &source,
            "abcdef1234567890",
            &home.join("bin/airc")
        )
        .is_err());
        assert!(!root.join("stopped").exists());
        // Explicit prebuilt input snapshots and verifies before maintenance,
        // even when compilation is unavailable. Both checksum and revision
        // failures refuse without falling back to Cargo.
        let input = root.join("downloaded binary");
        write(&input, good);
        let normal_shell = std::env::var_os("AIRC_HANDOFF_BASH").unwrap_or_else(|| bash.clone());
        let hash = process::background(&normal_shell)
            .args(["-c", "if command -v sha256sum >/dev/null; then sha256sum \"$AIRC_PREBUILT_ARTIFACT\"; else shasum -a 256 \"$AIRC_PREBUILT_ARTIFACT\"; fi"])
            .env("AIRC_PREBUILT_ARTIFACT", &input)
            .output().unwrap();
        assert!(hash.status.success());
        let hash = String::from_utf8(hash.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .trim_start_matches('\\')
            .to_owned();
        std::env::set_var("AIRC_PREBUILT_ARTIFACT", &input);
        let before = std::fs::read_to_string(root.join("events")).unwrap();
        for checksum in ["invalid".to_owned(), "0".repeat(64)] {
            std::env::set_var("AIRC_PREBUILT_SHA256", checksum);
            assert!(update_artifact::PreparedInstall::prepare(
                &bash,
                &source,
                "abcdef1234567890",
                &home.join("bin/airc")
            )
            .is_err());
        }
        std::env::set_var("AIRC_PREBUILT_SHA256", &hash);
        let prepared = update_artifact::PreparedInstall::prepare(
            &bash,
            &source,
            "abcdef1234567890",
            &home.join("bin/airc"),
        )
        .unwrap();
        // Enable the real integration phase while Cargo metadata is broken.
        // It must use the installed snapshot, not either stale Cargo output.
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        write(&home.join(".codex/config.toml"), "# fixture");
        write(&tools.join("codex"), "#!/bin/sh\nexit 0\n");
        write(&root.join("metadata-case"), "failure");
        std::env::remove_var("AIRC_SKIP_CODEX_HOOKS");
        // Replacing the input after prepare must not affect the installation.
        write(&input, "#!/bin/sh\necho 'build: deadbee'\n");
        prepared
            .install_after(|| {
                std::fs::write(root.join("stopped"), "daemon maintenance")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("bin/airc")).unwrap(),
            good
        );
        std::fs::remove_file(root.join("stopped")).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("events")).unwrap(),
            format!("{before}hook\n")
        );
        std::env::set_var("AIRC_SKIP_CODEX_HOOKS", "1");
        std::fs::remove_file(root.join("metadata-case")).unwrap();
        let before = std::fs::read_to_string(root.join("events")).unwrap();
        // Hash valid for wrong revision: verified bytes alone are insufficient.
        let wrong_hash = process::background(&normal_shell)
            .args(["-c", "if command -v sha256sum >/dev/null; then sha256sum \"$AIRC_PREBUILT_ARTIFACT\"; else shasum -a 256 \"$AIRC_PREBUILT_ARTIFACT\"; fi"])
            .output().unwrap();
        assert!(wrong_hash.status.success());
        std::env::set_var(
            "AIRC_PREBUILT_SHA256",
            String::from_utf8(wrong_hash.stdout)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .trim_start_matches('\\'),
        );
        assert!(update_artifact::PreparedInstall::prepare(
            &bash,
            &source,
            "abcdef1234567890",
            &home.join("bin/airc")
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("events")).unwrap(),
            before
        );
        assert!(!root.join("stopped").exists());
        std::env::remove_var("AIRC_PREBUILT_ARTIFACT");
        std::env::remove_var("AIRC_PREBUILT_SHA256");
        // Wrong expected source SHA is rejected before any build.
        let before = std::fs::read_to_string(root.join("events")).unwrap();
        assert!(update_artifact::PreparedInstall::prepare(
            &bash,
            &source,
            "deadbee",
            &home.join("bin/airc")
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("events")).unwrap(),
            before
        );
        // A normal public install adopts through the installed binary exactly
        // once. Failed adoption must prevent the final installation receipt.
        std::fs::remove_file(root.join("fail-build")).unwrap();
        write(&target.join("release/airc"), good);
        std::fs::create_dir_all(source.join("setup")).unwrap();
        write(&source.join("setup/github-auth.sh"), "#!/bin/sh\nexit 0\n");
        for fails in [false, true] {
            write(&root.join("events"), "");
            if fails {
                write(&root.join("fail-adopt"), "fail");
            }
            let normal_shell =
                std::env::var_os("AIRC_HANDOFF_BASH").unwrap_or_else(|| bash.clone());
            let output = process::background(normal_shell)
                .arg(source.join("install.sh"))
                .env("AIRC_DIR", &source)
                .env("AIRC_INSTALL_NO_PULL", "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.success(), !fails, "{stdout}\n{stderr}");
            assert_eq!(
                std::fs::read_to_string(root.join("events")).unwrap(),
                "build\nadopt\n",
                "normal installation must build before adopting exactly once"
            );
            assert_eq!(stdout.contains("Installed."), !fails);
            if fails {
                assert!(stderr.contains("Setup is incomplete"), "{stderr}");
            }
        }
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
    let process = process_exit::DaemonExit::capture(&pid_file).unwrap();
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
