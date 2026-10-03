//! A bounded, owned installer handoff. Compilation and artifact validation must
//! finish before the caller may enter its daemon maintenance window.
use std::ffi::OsStr;
#[cfg(not(windows))]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

type Error = Box<dyn std::error::Error>;

pub struct PreparedInstall {
    directory: PathBuf,
    artifact: PathBuf,
    source: PathBuf,
    #[cfg(not(windows))]
    shell: OsString,
    expected: String,
    bin_directory: PathBuf,
}

impl PreparedInstall {
    pub fn prepare(
        shell: &OsStr,
        source: &Path,
        expected: &str,
        executable: &Path,
    ) -> Result<Self, Error> {
        #[cfg(windows)]
        let _ = shell; // Native entry owns complete Git discovery on Windows.
        let bin_directory = executable
            .parent()
            .ok_or("executing updater has no installation directory")?
            .to_path_buf();
        let directory = std::env::temp_dir().join(format!(
            "airc-update-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&directory)?;
        let prepared = Self {
            artifact: directory.join(if cfg!(windows) { "airc.exe" } else { "airc" }),
            directory,
            source: source.to_path_buf(),
            #[cfg(not(windows))]
            shell: shell.to_os_string(),
            expected: expected.to_owned(),
            bin_directory,
        };
        println!("Preparing and verifying the update while the daemon stays up…");
        prepared.run("--prepare-artifact")?;
        if !prepared.artifact.is_file() {
            return Err("installer succeeded without producing the prepared artifact".into());
        }
        Ok(prepared)
    }

    /// The caller cannot stop the daemon until `prepare` has succeeded. All
    /// installer work here consumes the snapshot, never the shared Cargo cache.
    pub fn install_after(
        &self,
        before_install: impl FnOnce() -> Result<(), Error>,
    ) -> Result<(), Error> {
        before_install()?;
        self.run("--prebuilt")
    }

    pub(crate) fn artifact(&self) -> &Path {
        &self.artifact
    }

    fn run(&self, mode: &str) -> Result<(), Error> {
        #[cfg(windows)]
        let mut command = {
            let powershell = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .ok_or("SystemRoot is missing")?
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let mut command = airc_core::process::background(powershell);
            command
                .args(["-NoProfile", "-ExecutionPolicy", "RemoteSigned", "-File"])
                .arg(self.source.join("install.ps1"))
                .arg(if mode == "--prepare-artifact" {
                    "-PrepareArtifact"
                } else {
                    "-PrebuiltArtifact"
                })
                .arg(&self.artifact)
                .arg("-ExpectedBuild")
                .arg(&self.expected);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = airc_core::process::background(&self.shell);
            command
                .arg(self.source.join("install.sh"))
                .args([mode])
                .arg(&self.artifact)
                .arg("--expected-build")
                .arg(&self.expected);
            command
        };
        let status = command
            .env("AIRC_DIR", &self.source)
            .env("AIRC_INSTALL_NO_PULL", "1")
            // The transaction displaces this executing installation. Never
            // publish the replacement to another PATH entry or shell default.
            .env("BIN_DIR", &self.bin_directory)
            .env("BIN_TARGET", &self.bin_directory)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        if !status.success() {
            let entry = if cfg!(windows) {
                "install.ps1"
            } else {
                "install.sh"
            };
            return Err(format!("{entry} {mode} failed: {status}").into());
        }
        Ok(())
    }
}

impl Drop for PreparedInstall {
    fn drop(&mut self) {
        // Only this freshly-created temporary directory is owned here. The
        // source checkout, installed binary and Cargo outputs are never removed.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
