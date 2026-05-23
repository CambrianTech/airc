//! Codex detached-start adapter.
//!
//! Codex shell tool calls may clean up background children when the
//! command returns. This command launches `airc join` as a detached
//! process with explicit home/log state, replacing the former Python
//! `airc_core.codex_start` adapter.

use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};

pub async fn run(
    airc: &Path,
    home: &Path,
    log: &Path,
    join_args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    std::fs::create_dir_all(&home)?;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let stdout = OpenOptions::new().create(true).append(true).open(log)?;
    let stderr = stdout.try_clone()?;

    let mut command = Command::new(airc);
    command
        .arg("join")
        .args(normalize_join_args(join_args))
        .env("AIRC_HOME", &home)
        .env("AIRC_CODEX_START_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    detach(&mut command);
    let child = command.spawn()?;
    println!(
        "airc join: launched Codex-detached transport for {} (PID {}, log {})",
        home.display(),
        child.id(),
        log.display()
    );
    Ok(())
}

fn normalize_join_args(args: Vec<String>) -> Vec<String> {
    args.into_iter().filter(|arg| arg != "--").collect()
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: `pre_exec` runs in the child after fork and before exec. The
    // closure only calls `setsid` and constructs an OS error from errno on
    // failure; it does not allocate, lock, or touch shared process state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(test)]
mod tests {
    use super::normalize_join_args;

    #[test]
    fn normalize_join_args_strips_separator() {
        assert_eq!(
            normalize_join_args(vec!["--".into(), "--room".into(), "general".into()]),
            vec!["--room".to_string(), "general".to_string()]
        );
    }
}
