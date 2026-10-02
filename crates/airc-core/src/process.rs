//! Shared construction policy for background subprocesses.
//!
//! Preserve arguments, environment and standard streams; suppress only Windows
//! console allocation. Async callers convert this command with `Command::from`.
use std::ffi::OsStr;
use std::process::Command;

pub fn background(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    configure_background(&mut command);
    command
}

/// Preserve terminal attachment for explicitly interactive user commands.
pub fn interactive(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

pub fn configure_background(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod ownership_tests {
    #[test]
    fn migrated_launch_paths_cannot_bypass_shared_policy() {
        for source in [
            include_str!("../../airc-cli/src/update_artifact.rs"),
            include_str!("../../airc-cli/src/update_commands.rs"),
            include_str!("../../airc-cli/src/staleness.rs"),
            include_str!("../../airc-daemon/src/auto_update.rs"),
            include_str!("../../airc-cli/src/gh_client.rs"),
            include_str!("../../airc-transport/src/gh_gist/client.rs"),
            include_str!("../../airc-cli/src/channel_gist_commands.rs"),
            include_str!("../../airc-cli/src/client_id.rs"),
            include_str!("../../airc-cli/src/doctor/binary.rs"),
            include_str!("../../airc-cli/src/gh_commands.rs"),
            include_str!("../../airc-cli/src/gh_reqwest.rs"),
            include_str!("../../airc-cli/src/sos_commands.rs"),
            include_str!("../../airc-cli/src/work_commands_gh.rs"),
            include_str!("../../airc-cli/src/work_commands_git.rs"),
            include_str!("../../airc-cli/src/hygiene_commands.rs"),
            include_str!("../../airc-cli/src/identity_commands.rs"),
            include_str!("../../airc-cli/src/knock_commands.rs"),
            include_str!("../../airc-cli/src/queue_card_staleness.rs"),
            include_str!("../../airc-daemon/src/reclaim.rs"),
            include_str!("../../airc-work/src/local_git.rs"),
            include_str!("../../airc-work/src/pull_requests/gh.rs"),
            include_str!("../../airc-cli/src/cli.rs"),
            include_str!("../../airc-cli/src/work_commands.rs"),
            include_str!("../../airc-lib/src/work_worktree.rs"),
            include_str!("../../airc-lib/src/mesh_identity.rs"),
            include_str!("../../airc-cli/src/monitor/formatter.rs"),
        ] {
            assert!(
                !source.contains("Command::new("),
                "raw process constructor bypass"
            );
            assert!(
                !source.contains(".creation_flags("),
                "duplicated platform launch policy"
            );
        }
    }
}
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleWindow() -> *mut std::ffi::c_void;
    }

    const CHILD: &str = "process::tests::native_console_observer";

    #[test]
    #[ignore = "actual binary child invoked by the owning test"]
    fn native_console_observer() {
        // SAFETY: GetConsoleWindow takes no pointers and only queries this process.
        assert!(unsafe { GetConsoleWindow() }.is_null());
        if std::env::var("AIRC_WINDOW_TEST_ROLE").as_deref() == Ok("parent") {
            let result = background(std::env::current_exe().unwrap())
                .args(["--exact", CHILD, "--ignored", "--nocapture"])
                .env("AIRC_WINDOW_TEST_ROLE", "child")
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
            assert!(String::from_utf8_lossy(&result.stdout).contains("WINDOWLESS-VALUE"));
        }
        println!(
            "WINDOWLESS-VALUE:{}",
            std::env::var("AIRC_WINDOW_TEST_VALUE").unwrap()
        );
        eprintln!("WINDOWLESS-STDERR");
        if std::env::var("AIRC_WINDOW_TEST_EXIT").as_deref() == Ok("23") {
            std::process::exit(23);
        }
    }

    #[test]
    fn consoleless_updater_children_preserve_output_and_exit() {
        let output = background(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("AIRC_WINDOW_TEST_ROLE", "parent")
            .env("AIRC_WINDOW_TEST_VALUE", "spaces ' quotes \" and λ")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("spaces ' quotes \" and λ"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("WINDOWLESS-STDERR"));
        let failure = background(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("AIRC_WINDOW_TEST_ROLE", "child")
            .env("AIRC_WINDOW_TEST_VALUE", "failure")
            .env("AIRC_WINDOW_TEST_EXIT", "23")
            .output()
            .unwrap();
        assert_eq!(failure.status.code(), Some(23));
        assert!(String::from_utf8_lossy(&failure.stderr).contains("WINDOWLESS-STDERR"));
    }

    #[test]
    fn ci_git_bash_descendant_preserves_windowless_updater() {
        // MSYS owns the native child boundary. Only CI may exercise its default
        // spawn policy: a regression could allocate a window before detection.
        if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
            eprintln!("SKIP CI-only default Git Bash descendant visibility probe");
            return;
        }
        let git = background("git").arg("--exec-path").output().unwrap();
        assert!(git.status.success());
        let exec = std::path::PathBuf::from(String::from_utf8(git.stdout).unwrap().trim());
        let bash = exec
            .ancestors()
            .find_map(|root| {
                ["bin/bash.exe", "usr/bin/bash.exe"]
                    .iter()
                    .map(|p| root.join(p))
                    .find(|p| p.is_file())
            })
            .expect("Git for Windows Bash");
        let output = background(bash)
            .args(["--noprofile", "--norc", "-c", "\"$AIRC_WINDOW_TEST_EXE\" --exact \"$AIRC_WINDOW_TEST_CASE\" --ignored --nocapture"])
            .env("AIRC_WINDOW_TEST_EXE", std::env::current_exe().unwrap().to_str().unwrap().replace('\\', "/"))
            .env("AIRC_WINDOW_TEST_CASE", CHILD)
            .env("AIRC_WINDOW_TEST_ROLE", "child")
            .env("AIRC_WINDOW_TEST_VALUE", "bash-child")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Bash allocated a descendant console: {output:?}"
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("WINDOWLESS-VALUE:bash-child"));
    }
}
