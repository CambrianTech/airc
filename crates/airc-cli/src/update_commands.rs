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
        .ok_or("HOME is not set; cannot resolve ~/.airc/src")?;
    Ok(PathBuf::from(home).join(".airc").join("src"))
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
        Command::new("bash")
            .arg(source.join("install.sh"))
            .env("AIRC_DIR", source)
            .env("AIRC_INSTALL_NO_PULL", "1"),
        "install.sh",
    )
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
