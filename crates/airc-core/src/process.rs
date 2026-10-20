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

/// Windows daemon lifecycle policy; Unix session ownership stays with callers.
#[cfg(windows)]
pub fn configure_detached(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS and CREATE_NEW_PROCESS_GROUP replace CREATE_NO_WINDOW:
    // Windows ignores CREATE_NO_WINDOW when DETACHED_PROCESS is present.
    command.creation_flags(0x0000_0008 | 0x0000_0200);
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

/// Why [`run_bounded`] produced no output.
#[derive(Debug)]
pub enum BoundedError {
    /// The program could not be started.
    Spawn(std::io::Error),
    /// Waiting on it failed.
    Wait(std::io::Error),
    /// It outlived its bound; its whole process group was killed.
    TimedOut(std::time::Duration),
}

impl std::fmt::Display for BoundedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "spawn failed: {error}"),
            Self::Wait(error) => write!(f, "wait failed: {error}"),
            Self::TimedOut(bound) => {
                write!(
                    f,
                    "did not finish within {} s and was killed",
                    bound.as_secs()
                )
            }
        }
    }
}

impl std::error::Error for BoundedError {}

/// Run `command` to completion within `bound`: stdin closed (it can never wait on a
/// prompt), stdout and stderr drained while it runs (a chatty child can never block on a
/// full pipe), polled every 50 ms. On timeout the child's WHOLE process group is killed,
/// not just the child: `git fetch` runs `git-remote-https` as its own child, and killing
/// only `git` left that helper holding the network and the lock (card c76d382a). The one
/// bounded runner; callers never hand-roll the poll-and-kill loop.
pub fn run_bounded(
    mut command: Command,
    bound: std::time::Duration,
) -> Result<std::process::Output, BoundedError> {
    use std::io::Read;
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own group, so the timeout can end everything it started.
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(BoundedError::Spawn)?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes); // a read error ends the drain; the exit status says what happened
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = std::time::Instant::now() + bound;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                kill_tree(&mut child);
                let _ = child.wait(); // reaps the killed child; its exit status is the timeout
                return Err(BoundedError::TimedOut(bound));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(error) => {
                kill_tree(&mut child);
                return Err(BoundedError::Wait(error));
            }
        }
    };
    Ok(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(), // unwrap_or_default: a panicked drain thread read nothing
        stderr: stderr.join().unwrap_or_default(), // unwrap_or_default: a panicked drain thread read nothing
    })
}

/// End `child` and everything it started.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child leads its own group (`process_group(0)` above), so its pid is the
        // pgid, and `kill -- -<pgid>` signals the whole group. A command, not FFI: this
        // file is also compiled into airc-cli's build script, which links no libc.
        let _ = background("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(); // kill failing leaves child.kill() below as the last word
    }
    #[cfg(windows)]
    {
        // No process groups to signal here: end the process TREE by parent id.
        let _ = background("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(); // taskkill failing leaves child.kill() below as the last word
    }
    let _ = child.kill(); // already dead after the group kill; this covers any host where that did not reach it
}

#[cfg(all(test, unix))]
mod bounded_tests {
    use super::*;
    use std::time::Duration;

    // what this catches: a timeout that kills only the direct child, leaving what it started
    // running (card c76d382a: git-remote-https outlived its git), or a success path that
    // loses output or blocks on a full pipe.
    #[test]
    fn a_timeout_ends_the_whole_process_group_and_success_keeps_all_output() {
        let dir = std::env::temp_dir().join(format!("airc-bounded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("grandchild.pid");
        let mut sh = background("/bin/sh");
        sh.args([
            "-c",
            &format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
        ]);
        let started = std::time::Instant::now();
        assert!(matches!(
            run_bounded(sh, Duration::from_millis(400)),
            Err(BoundedError::TimedOut(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(5), "the bound held");
        let grandchild = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .to_string();
        let alive = || {
            background("kill")
                .args(["-0", &grandchild])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        let gone_by = std::time::Instant::now() + Duration::from_secs(3);
        while alive() && std::time::Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !alive(),
            "the grandchild the child started was killed with it"
        );
        let _ = std::fs::remove_dir_all(&dir);

        let mut big = background("/bin/sh");
        big.args(["-c", "head -c 200000 /dev/zero | tr '\\0' x; echo done >&2"]);
        let out = run_bounded(big, Duration::from_secs(20)).expect("finishes");
        assert!(out.status.success());
        assert_eq!(
            out.stdout.len(),
            200_000,
            "past a pipe buffer, nothing lost or blocked"
        );
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "done");
    }
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
    fn detached_child_preserves_explicit_output_without_console() {
        let mut child = background(std::env::current_exe().unwrap());
        configure_detached(&mut child);
        let output = child
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("AIRC_WINDOW_TEST_ROLE", "child")
            .env("AIRC_WINDOW_TEST_VALUE", "detached")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("WINDOWLESS-VALUE:detached"));
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
