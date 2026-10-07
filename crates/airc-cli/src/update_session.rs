//! One existing native setup owner covers both updater phases on Windows.
use std::path::Path;

type Error = Box<dyn std::error::Error>;
const RESTORED: u8 = 200;

#[derive(Debug)]
struct SessionExit(u8);
impl std::fmt::Display for SessionExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Windows update session exited {}", self.0)
    }
}
impl std::error::Error for SessionExit {}

pub(crate) fn exit_code(error: &(dyn std::error::Error + 'static)) -> Option<u8> {
    error.downcast_ref::<SessionExit>().map(|error| error.0)
}

pub(crate) fn run(home: &Path, auto: bool) -> Result<(), Error> {
    let source = crate::update_commands::install_source_dir()?;
    let wrapper = source.join("windows/install-session.ps1");
    if !wrapper.is_file() {
        return Err(
            "Install source lacks the Windows updater session adapter; rerun the public installer"
                .into(),
        );
    }
    let powershell = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .ok_or("SystemRoot is missing")?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = airc_core::process::background(powershell);
    command
        .args(["-NoProfile", "-ExecutionPolicy", "RemoteSigned", "-File"])
        .arg(&wrapper)
        .arg("-SourceDirectory")
        .arg(&source);
    let borrowed = std::env::var_os("CAMBRIAN_INSTALL_ELEVATION").is_some_and(|v| !v.is_empty());
    if borrowed {
        // The existing helper verifies owner start time and actual ancestry;
        // environment presence alone never authorizes a borrowed session.
        let validation = command.arg("-ValidateOnly").status()?;
        if !validation.success() {
            return Err("Inherited Windows update owner failed validation".into());
        }
        let socket = crate::cli::default_socket_path_in(home);
        let result = if auto {
            crate::update_commands::run_update_auto(home, socket)
        } else {
            crate::update_commands::run_update(home, socket)
        };
        return result.map_err(|error| {
            if std::env::var_os("AIRC_UPDATE_SESSION_OWNER").is_some_and(|v| !v.is_empty())
                && crate::update_commands::restored_runtime_verified(error.as_ref())
            {
                eprintln!("airc: {error}");
                Box::new(SessionExit(RESTORED)) as Error
            } else {
                error
            }
        });
    }
    if std::env::var_os("AIRC_UPDATE_SESSION_OWNER").is_some_and(|v| !v.is_empty()) {
        return Err("Updater owner marker exists without its elevation context".into());
    }
    command
        .arg("-UpdaterPath")
        .arg(std::env::current_exe()?)
        .arg("-HomePath")
        .arg(home);
    if auto {
        command.arg("-AutoUpdate");
    }
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Box::new(SessionExit(
            status
                .code()
                .and_then(|c| u8::try_from(c).ok())
                .unwrap_or(1),
        )))
    }
}
