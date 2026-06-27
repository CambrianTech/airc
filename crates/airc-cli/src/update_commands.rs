use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub fn run_update(home: &Path, socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let source = install_source_dir()?;
    validate_source_checkout(&source)?;
    let airc_exe = env::current_exe()?;
    let daemon_was_running = daemon_is_running(&airc_exe, home, &socket)?;

    let branch = git_text(&source, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" || branch.is_empty() {
        return Err(format!(
            "install source is detached at {}; check out a branch before updating",
            source.display()
        )
        .into());
    }

    let before = git_text(&source, ["rev-parse", "--short", "HEAD"])?;
    if daemon_was_running {
        stop_daemon(&airc_exe, home, &socket)?;
    }
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .arg("fetch")
            .arg("--quiet")
            .arg("origin")
            .arg(&branch),
        "git fetch",
    )?;
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .arg("pull")
            .arg("--ff-only")
            .arg("--quiet"),
        "git pull --ff-only",
    )?;
    let after = git_text(&source, ["rev-parse", "--short", "HEAD"])?;

    run_installer(&source)?;

    if before == after {
        println!("Already at {after}.");
    } else {
        println!("Updated: {before} -> {after}");
    }
    if daemon_was_running {
        restart_daemon(&airc_exe, home, &socket)?;
        wait_daemon_ready(&airc_exe, home, &socket)?;
        println!("daemon: restarted.");
    }
    Ok(())
}

/// `airc update --auto` — self-update with a smoke-test and rollback.
///
/// The safe sibling of [`run_update`]: it backs up the live binary,
/// rebuilds, runs the new binary through a smoke-test, and ROLLS BACK to
/// the backup if the new build is broken. So an auto-update can never
/// leave a peer with a binary that compiles-but-doesn't-run.
///
/// Flow: fetch + ff-pull the channel → if HEAD unchanged, nothing to do
/// → else back up the installed binary to `airc.prev`, rebuild in place,
/// smoke-test (the new binary's `version` reports the pulled SHA), and on
/// failure restore `airc.prev`.
///
/// Platform note: on Windows the live `airc.exe` is locked while this
/// process runs, so the in-place reinstall (and thus the swap) inherits
/// the same constraint as `run_update` — most valuable on the macOS /
/// Linux grid nodes today. The rollback path is a no-op there because no
/// swap occurred.
pub fn run_update_auto(home: &Path, socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let source = install_source_dir()?;
    validate_source_checkout(&source)?;
    let airc_exe = env::current_exe()?;
    let daemon_was_running = daemon_is_running(&airc_exe, home, &socket)?;

    let branch = git_text(&source, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" || branch.is_empty() {
        return Err(format!(
            "install source is detached at {}; check out a branch before auto-updating",
            source.display()
        )
        .into());
    }
    let before = git_text(&source, ["rev-parse", "--short", "HEAD"])?;

    if daemon_was_running {
        stop_daemon(&airc_exe, home, &socket)?;
    }
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["fetch", "--quiet", "origin", &branch]),
        "git fetch",
    )?;
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(&source)
            .args(["pull", "--ff-only", "--quiet"]),
        "git pull --ff-only",
    )?;
    let after = git_text(&source, ["rev-parse", "--short", "HEAD"])?;

    if before == after {
        println!("Already at {after} — nothing to auto-update.");
        if daemon_was_running {
            restart_daemon(&airc_exe, home, &socket)?;
            wait_daemon_ready(&airc_exe, home, &socket)?;
        }
        return Ok(());
    }

    // Back up the live binary BEFORE the rebuild — this is the rollback
    // anchor. Copying a running exe for read is allowed on every platform.
    let prev = airc_exe.with_file_name("airc.prev");
    std::fs::copy(&airc_exe, &prev).map_err(|e| {
        format!(
            "could not back up the current binary to {}: {e}",
            prev.display()
        )
    })?;

    let installed = run_installer(&source);

    // Smoke-test: the new binary must RUN and report the SHA we pulled —
    // a build that compiled but is broken (or didn't actually replace the
    // binary) fails here and triggers rollback.
    let smoke_ok = installed.is_ok() && smoke_test_new_binary(&airc_exe, &after);

    if smoke_ok {
        println!(
            "Auto-updated: {before} -> {after} (smoke-test passed; backup at {}).",
            prev.display()
        );
        if daemon_was_running {
            restart_daemon(&airc_exe, home, &socket)?;
            wait_daemon_ready(&airc_exe, home, &socket)?;
        }
        Ok(())
    } else {
        eprintln!("⚠ new build did not pass the smoke-test — ROLLING BACK to the previous binary.");
        // Restore the known-good binary. (No-op-safe on Windows where the
        // reinstall couldn't replace the locked live exe in the first place.)
        if let Err(e) = std::fs::copy(&prev, &airc_exe) {
            return Err(format!(
                "auto-update FAILED and rollback ALSO failed ({e}); \
                 your previous binary is at {} — restore it manually",
                prev.display()
            )
            .into());
        }
        if daemon_was_running {
            // Restart on the rolled-back (known-good) binary.
            let _ = restart_daemon(&airc_exe, home, &socket);
            let _ = wait_daemon_ready(&airc_exe, home, &socket);
        }
        Err(format!(
            "auto-update rolled back: the {after} build failed the smoke-test; \
             restored the previous binary ({before})"
        )
        .into())
    }
}

/// Run the freshly-installed binary's `version` and confirm it reports
/// the `expected_short` SHA we just pulled — proof the new binary RUNS
/// and is the build we intended. Any failure (won't run, wrong/old SHA,
/// unparseable) returns false → the caller rolls back.
fn smoke_test_new_binary(airc_exe: &Path, expected_short: &str) -> bool {
    let Ok(output) = Command::new(airc_exe).arg("version").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_build_sha(&stdout) {
        Some(sha) => smoke_sha_matches(&sha, expected_short),
        None => false,
    }
}

/// Parse the build SHA from `airc version` output (the token after the
/// `build:` label). Pure — unit-tested.
fn parse_build_sha(version_stdout: &str) -> Option<String> {
    for line in version_stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("build:") {
            return rest.split_whitespace().next().map(|s| s.to_string());
        }
    }
    None
}

/// Whether the installed binary's SHA matches the pulled short SHA.
/// Tolerant of differing short-SHA lengths (git `--short` vs the version
/// banner's 12-char form) via a prefix match either direction.
fn smoke_sha_matches(installed_sha: &str, expected_short: &str) -> bool {
    !installed_sha.is_empty()
        && !expected_short.is_empty()
        && (installed_sha.starts_with(expected_short) || expected_short.starts_with(installed_sha))
}

fn daemon_is_running(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(daemon_command(airc_exe, home, "ping", socket)
        .output()?
        .status
        .success())
}

fn stop_daemon(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = daemon_command(airc_exe, home, "stop", socket);
    run_checked(&mut command, "airc stop before update")
}

fn restart_daemon(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(home)?;
    let log = home.join("airc-daemon.log");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    let stderr = stdout.try_clone()?;
    let mut command = daemon_command(airc_exe, home, "daemon", socket);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    detach_daemon(&mut command);
    command.spawn()?;
    Ok(())
}

fn wait_daemon_ready(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // what this catches: the same cold-start-too-short bug #1211 fixed
    // for `ensure_daemon_running` (5s → 20s). A freshly rebuilt daemon
    // re-runs SQLite migrations + identity load + substrate `Airc::open`
    // on boot before it binds its IPC socket; on a cold/slow machine
    // that exceeds 5s, so `airc update` reported "daemon did not become
    // ready" even though the daemon came up moments later — leaving the
    // node on the OLD build. 20s comfortably covers a cold boot while
    // still surfacing a genuinely dead daemon.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if daemon_is_running(airc_exe, home, socket)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon did not become ready after update: {}",
                home.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn daemon_command(airc_exe: &Path, home: &Path, subcommand: &str, socket: &Path) -> Command {
    let mut command = Command::new(airc_exe);
    command
        .arg("--home")
        .arg(home)
        .arg(subcommand)
        .arg("--socket")
        .arg(socket);
    command
}

#[cfg(unix)]
fn detach_daemon(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: this closure runs in the child just before exec and
    // only calls setsid, which is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach_daemon(_command: &mut Command) {}

pub(crate) fn install_source_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = env::var_os("AIRC_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or("HOME is not set; cannot resolve the airc install source")?;
    let home = PathBuf::from(home);
    // install.sh / install.ps1 record the dir they actually installed
    // FROM in this marker. install.sh's _default_clone_dir installs from a
    // dev checkout (e.g. ~/work/airc) when run inside one — and
    // rust-rewrite currently ships ONLY as a dev checkout (no release
    // channel yet) — so the source is frequently NOT ~/.airc/src. Without
    // the marker, `airc update` died with "No git checkout at
    // ~/.airc/src" for every dev-checkout install (caught live
    // 2026-06-13). Honor the marker before falling back to the default.
    if let Some(recorded) = read_install_source_marker(&home) {
        return Ok(recorded);
    }
    Ok(home.join(".airc").join("src"))
}

/// Read the path recorded in `~/.airc/install-source`, if present and
/// non-empty. Returns `None` on any read error or blank content so the
/// caller falls back to the default location.
fn read_install_source_marker(home: &Path) -> Option<PathBuf> {
    let marker = home.join(".airc").join("install-source");
    let contents = std::fs::read_to_string(&marker).ok()?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn validate_source_checkout(source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !source.join(".git").exists() {
        return Err(format!(
            "No git checkout at {}. Reinstall airc from the install script.",
            source.display()
        )
        .into());
    }
    if !source.join("install.sh").is_file() {
        return Err(format!(
            "install source {} is missing install.sh; reinstall airc from the install script",
            source.display()
        )
        .into());
    }
    Ok(())
}

fn run_installer(source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Stream the installer's output live instead of buffering it with
    // `Command::output()` (what run_checked does). install.sh does the
    // cargo rebuild, which can take minutes; with buffered stdio the
    // operator saw NOTHING while it ran, so a long-or-hung build was
    // indistinguishable from a working one — on a live Windows node
    // `airc update` "hung" silently for 15+ minutes with no visible
    // progress. Inheriting stdio surfaces cargo's progress live; the
    // banner sets the expectation up front. Failure behavior is
    // unchanged: a non-zero exit still returns an Err.
    println!("Rebuilding airc (this can take a few minutes)…");
    let status = Command::new(installer_shell())
        .arg(source.join("install.sh"))
        .env("AIRC_DIR", source)
        .env("AIRC_INSTALL_NO_PULL", "1")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if status.success() {
        return Ok(());
    }
    Err(format!("install.sh failed: exit status {status}").into())
}

/// The shell used to run install.sh during `airc update`.
///
/// On Windows, a plain `bash` resolves to `C:\Windows\System32\bash.exe`
/// — the WSL launcher — which fails with "Windows Subsystem for Linux has
/// no installed distributions" when no distro is present, so `airc
/// update` died at the reinstall step (caught live 2026-06-13). Prefer
/// the Git-for-Windows bash derived from `git --exec-path` (git is an
/// airc prereq). On Unix there is no `bin/bash.exe`, so this finds
/// nothing and the caller falls back to plain `bash` — unchanged.
fn installer_shell() -> std::ffi::OsString {
    if let Some(bash) = git_bundled_bash() {
        return bash.into_os_string();
    }
    std::ffi::OsString::from("bash")
}

fn git_bundled_bash() -> Option<PathBuf> {
    let output = Command::new("git").arg("--exec-path").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let exec = String::from_utf8(output.stdout).ok()?;
    bash_in_git_root(Path::new(exec.trim()))
}

/// Walk up from a git exec-path (e.g. `.../Git/mingw64/libexec/git-core`)
/// looking for a bundled `bin/bash.exe` or `usr/bin/bash.exe` under any
/// ancestor. Pure path logic so it is testable on every platform.
fn bash_in_git_root(exec_path: &Path) -> Option<PathBuf> {
    exec_path.ancestors().find_map(|root| {
        ["bin/bash.exe", "usr/bin/bash.exe"]
            .iter()
            .map(|rel| root.join(rel))
            .find(|candidate| candidate.is_file())
    })
}

fn git_text<const N: usize>(
    source: &Path,
    args: [&str; N],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(command_error("git", &output).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn run_checked(
    command: &mut Command,
    label: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_error(label, &output).into())
}

fn command_error(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };
    format!("{label} failed: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_source_prefers_airc_dir() {
        temp_env::with_vars(
            [
                ("AIRC_DIR", Some("/tmp/custom-airc")),
                ("HOME", Some("/tmp/home")),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/tmp/custom-airc")
                );
            },
        );
    }

    #[test]
    fn install_source_defaults_to_home_airc_src() {
        temp_env::with_vars(
            [
                ("AIRC_DIR", None::<&str>),
                ("HOME", Some("/tmp/home")),
                ("USERPROFILE", Some("/tmp/userprofile")),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/tmp/home/.airc/src")
                );
            },
        );
    }

    #[test]
    fn install_source_reads_marker_when_no_env() {
        // what this catches: airc update finding a dev-checkout install
        // source recorded by install.sh, instead of dying on ~/.airc/src
        // (regression for the 2026-06-13 "No git checkout" dev-install bug).
        let temp = tempfile::TempDir::new().unwrap();
        let airc = temp.path().join(".airc");
        std::fs::create_dir_all(&airc).unwrap();
        std::fs::write(airc.join("install-source"), "/opt/dev/airc\n").unwrap();
        temp_env::with_vars(
            [
                ("AIRC_DIR", None::<&str>),
                ("HOME", Some(temp.path().to_str().unwrap())),
                ("USERPROFILE", None::<&str>),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/opt/dev/airc")
                );
            },
        );
    }

    #[test]
    fn install_source_airc_dir_beats_marker() {
        // what this catches: explicit AIRC_DIR must still win over a
        // recorded marker (precedence order regression).
        let temp = tempfile::TempDir::new().unwrap();
        let airc = temp.path().join(".airc");
        std::fs::create_dir_all(&airc).unwrap();
        std::fs::write(airc.join("install-source"), "/opt/dev/airc\n").unwrap();
        temp_env::with_vars(
            [
                ("AIRC_DIR", Some("/explicit/override")),
                ("HOME", Some(temp.path().to_str().unwrap())),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/explicit/override")
                );
            },
        );
    }

    #[test]
    fn install_source_blank_marker_falls_back_to_default() {
        // what this catches: a blank/whitespace marker must not resolve to
        // an empty path; fall through to ~/.airc/src.
        let temp = tempfile::TempDir::new().unwrap();
        let airc = temp.path().join(".airc");
        std::fs::create_dir_all(&airc).unwrap();
        std::fs::write(airc.join("install-source"), "  \n").unwrap();
        temp_env::with_vars(
            [
                ("AIRC_DIR", None::<&str>),
                ("HOME", Some(temp.path().to_str().unwrap())),
                ("USERPROFILE", None::<&str>),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    temp.path().join(".airc").join("src")
                );
            },
        );
    }

    #[test]
    fn bash_in_git_root_finds_bundled_bash() {
        // what this catches: airc update finding Git-for-Windows' bash via
        // git --exec-path instead of invoking the System32 WSL launcher
        // (regression for the 2026-06-13 "WSL has no distributions" bug).
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("mingw64").join("libexec").join("git-core")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join("bash.exe"), b"#!/bin/sh\n").unwrap();
        let exec = root.join("mingw64").join("libexec").join("git-core");
        assert_eq!(
            bash_in_git_root(&exec).unwrap(),
            root.join("bin").join("bash.exe")
        );
    }

    #[test]
    fn bash_in_git_root_none_when_absent() {
        // what this catches: no false positive when no bundled bash exists,
        // so installer_shell falls back to plain `bash` (Unix path).
        let temp = tempfile::TempDir::new().unwrap();
        let exec = temp.path().join("mingw64").join("libexec").join("git-core");
        assert!(bash_in_git_root(&exec).is_none());
    }

    #[test]
    fn validate_source_requires_git_checkout() {
        let temp = tempfile::TempDir::new().unwrap();
        let error = validate_source_checkout(temp.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("No git checkout"));
    }

    #[test]
    fn daemon_command_passes_home_subcommand_and_socket() {
        let command = daemon_command(
            Path::new("/bin/airc"),
            Path::new("/tmp/home/.airc"),
            "daemon",
            Path::new("/tmp/airc.sock"),
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "--home",
                "/tmp/home/.airc",
                "daemon",
                "--socket",
                "/tmp/airc.sock"
            ]
        );
    }

    // what this catches: the smoke-test parses the build SHA from real
    // `airc version` output (the `build:` line), so a successful rebuild
    // can be verified to actually BE the build we pulled.
    #[test]
    fn parse_build_sha_reads_the_version_banner() {
        let out =
            "  airc 0.1.0\n  install: /home/u/.local/bin/airc\n  build:   9cc678fc0203 on canary\n";
        assert_eq!(parse_build_sha(out).as_deref(), Some("9cc678fc0203"));
        assert_eq!(parse_build_sha("no build line here"), None);
    }

    // what this catches: the SHA match tolerates the differing short-SHA
    // lengths (git --short ~7 chars vs the 12-char version banner) via a
    // prefix match either direction — and rejects a mismatch (the
    // rollback trigger) and empties.
    #[test]
    fn smoke_sha_matches_tolerates_short_sha_lengths() {
        assert!(smoke_sha_matches("9cc678fc0203", "9cc678f")); // banner longer
        assert!(smoke_sha_matches("9cc678f", "9cc678fc0203")); // pulled longer
        assert!(smoke_sha_matches("abcd1234", "abcd1234"));
        assert!(!smoke_sha_matches("9cc678fc0203", "deadbeef")); // mismatch -> rollback
        assert!(!smoke_sha_matches("", "9cc678f"));
        assert!(!smoke_sha_matches("9cc678f", ""));
    }
}
