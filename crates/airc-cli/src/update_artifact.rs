//! A bounded, owned installer handoff. Compilation and artifact validation must
//! finish before the caller may enter its daemon maintenance window.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Error = Box<dyn std::error::Error>;

pub struct PreparedInstall {
    directory: PathBuf,
    artifact: PathBuf,
    source: PathBuf,
    shell: OsString,
    expected: String,
}

impl PreparedInstall {
    pub fn prepare(shell: &OsStr, source: &Path, expected: &str) -> Result<Self, Error> {
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
            shell: shell.to_os_string(),
            expected: expected.to_owned(),
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

    fn run(&self, mode: &str) -> Result<(), Error> {
        let status = Command::new(&self.shell)
            .arg(self.source.join("install.sh"))
            .args([mode])
            .arg(&self.artifact)
            .arg("--expected-build")
            .arg(&self.expected)
            .env("AIRC_DIR", &self.source)
            .env("AIRC_INSTALL_NO_PULL", "1")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        if !status.success() {
            return Err(format!("install.sh {mode} failed: {status}").into());
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
