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
    let deadline = Instant::now() + Duration::from_secs(5);
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
    run_checked(
        Command::new(installer_shell())
            .arg(source.join("install.sh"))
            .env("AIRC_DIR", source)
            .env("AIRC_INSTALL_NO_PULL", "1"),
        "install.sh",
    )
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
}
