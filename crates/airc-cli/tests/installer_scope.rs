//! Public installer adoption must not borrow a foreign checkout's account.
mod common;

use std::{path::Path, process::Command};

fn isolated(command: &mut Command, account: &Path, cwd: &Path) {
    airc_core::process::configure_background(command);
    command
        .current_dir(cwd)
        .env("HOME", account)
        .env("USERPROFILE", account)
        .env_remove("AIRC_HOME")
        .env_remove("AIRC_RUNTIME_DIR")
        .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
        .env("AIRC_NO_STALENESS", "1");
}

#[test]
fn public_adoption_ignores_foreign_cwd_and_preserves_explicit_scope() {
    for explicit in [false, true] {
        let temp = common::daemon_tempdir();
        let account = temp.path().join("isolated-account");
        let foreign = temp.path().join("foreign/.airc");
        let cwd = foreign.join("worktrees/project");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let sentinel = foreign.join("events.sqlite");
        std::fs::write(&sentinel, b"foreign account must not be opened").unwrap();
        let before = std::fs::metadata(&sentinel).unwrap().modified().unwrap();
        let scope = if explicit {
            temp.path().join("explicit-scope")
        } else {
            account.join(".airc")
        };
        let exe = env!("CARGO_BIN_EXE_airc");

        #[cfg(windows)]
        let mut adoption = {
            // The generic Rust runner can be elevated. Exercise the real
            // selection/callsite with a temporary daemon here, without faking
            // Test-IsAdmin. Full-entry normal-token acceptance belongs to the
            // separate real standard-account clean-install job.
            let source = include_str!("../../../windows/adopt-installed.ps1");
            let scope_lines = source
                .lines()
                .filter(|line| {
                    line.trim_start()
                        .starts_with("$ScopeHome=Get-AircInstallerHome")
                        || line.trim_start().starts_with("$scopeArguments=")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let invoke = source
                .lines()
                .find(|line| line.contains("-PreserveChildrenOnSuccess $AircPath"))
                .unwrap();
            let mut command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-ExecutionPolicy", "RemoteSigned", "-Command"]);
            command.arg(format!("$ErrorActionPreference='Stop'; . $env:AIRC_SCOPE_TEST_HELPER; $AircPath=$env:AIRC_SCOPE_TEST_EXE; {scope_lines}\n{invoke}\nexit $LASTEXITCODE"));
            command.env(
                "AIRC_SCOPE_TEST_HELPER",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../windows/shared-setup.ps1"),
            );
            command.env("AIRC_SCOPE_TEST_EXE", exe);
            command
        };
        #[cfg(not(windows))]
        let mut adoption = {
            // Execute the actual POSIX public entry's assignment and invocation,
            // not a hand-written equivalent that can drift from the installer.
            let source = include_str!("../../../install.sh");
            let assignment = source
                .lines()
                .find(|line| line.trim_start().starts_with("installer_home="))
                .unwrap();
            let invoke = source
                .lines()
                .find(|line| {
                    line.trim_start()
                        .starts_with("*) \"$installed_airc\" --home")
                })
                .unwrap();
            let invoke = invoke
                .trim()
                .strip_prefix("*) ")
                .unwrap()
                .strip_suffix(" ;;")
                .unwrap();
            let mut command = Command::new("bash");
            command.args([
                "--noprofile",
                "--norc",
                "-c",
                &format!("fail() {{ echo \"$*\" >&2; exit 1; }}\n{assignment}\n{invoke}"),
            ]);
            command.env("installed_airc", exe);
            command
        };
        isolated(&mut adoption, &account, &cwd);
        if explicit {
            adoption.env("AIRC_HOME", &scope);
            #[cfg(windows)]
            {
                // A direct PowerShell entry can inherit MSYS HOME/AIRC_HOME.
                // Exercise the existing cygpath conversion with an actual path.
                let git = airc_core::process::background("git")
                    .arg("--exec-path")
                    .output()
                    .unwrap();
                assert!(git.status.success(), "Git discovery failed: {git:?}");
                let exec_path =
                    std::path::PathBuf::from(String::from_utf8(git.stdout).unwrap().trim());
                let converter = exec_path
                    .ancestors()
                    .map(|root| root.join("usr/bin/cygpath.exe"))
                    .find(|path| path.is_file())
                    .expect("Git installation has cygpath");
                let converted = airc_core::process::background(&converter)
                    .arg("-u")
                    .arg(&scope)
                    .output()
                    .unwrap();
                assert!(converted.status.success(), "cygpath failed: {converted:?}");
                adoption.env(
                    "AIRC_HOME",
                    String::from_utf8(converted.stdout).unwrap().trim(),
                );
                let mut paths = vec![converter.parent().unwrap().to_path_buf()];
                paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
                adoption.env("PATH", std::env::join_paths(paths).unwrap());
            }
        }
        let output = adoption.output().unwrap();
        assert!(
            output.status.success(),
            "public adoption failed: {:?}",
            output
        );
        assert!(
            scope.join("events.sqlite").exists(),
            "selected scope has no database: {}",
            scope.display()
        );

        let mut endpoint = Command::new(exe);
        isolated(&mut endpoint, &account, &cwd);
        endpoint.arg("--home").arg(&scope).arg("ipc-endpoint");
        let endpoint = endpoint.output().unwrap();
        assert!(endpoint.status.success());
        let endpoint = String::from_utf8(endpoint.stdout).unwrap();
        // Resolve the expected endpoint in the SAME isolated environment from
        // a neutral account cwd. Parent-process HOME would describe a different
        // machine-account boundary on Windows and is not a valid expectation.
        let mut expected = Command::new(exe);
        isolated(&mut expected, &account, &account);
        if explicit {
            expected.env("AIRC_HOME", &scope);
        }
        let expected = expected.arg("ipc-endpoint").output().unwrap();
        assert!(expected.status.success());
        assert_eq!(
            endpoint.trim(),
            String::from_utf8(expected.stdout).unwrap().trim()
        );
        // Ping never starts an absent owner, so this observes the daemon that
        // the actual public adapter adopted, not a replacement from a probe.
        let mut ping = Command::new(exe);
        isolated(&mut ping, &account, &cwd);
        ping.arg("--home").arg(&scope).arg("ping");
        assert!(ping.output().unwrap().status.success());
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"foreign account must not be opened"
        );
        assert_eq!(
            std::fs::metadata(&sentinel).unwrap().modified().unwrap(),
            before
        );
        let entries: Vec<_> = std::fs::read_dir(&foreign)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 2, "foreign state was created: {entries:?}");
        let mut stop = Command::new(exe);
        isolated(&mut stop, &account, &cwd);
        stop.arg("--home").arg(&scope).arg("stop");
        assert!(stop.output().unwrap().status.success());
    }
}
