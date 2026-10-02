//! Owned binary displacement and rollback; never overwrite a mapped executable.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

type Error = Box<dyn std::error::Error>;

pub(crate) struct BinarySwap {
    current: PathBuf,
    previous: PathBuf,
    failed: PathBuf,
    original_hash: [u8; 32],
    candidate_hash: [u8; 32],
}

impl BinarySwap {
    pub(crate) fn displace(current: &Path, candidate: &Path) -> Result<Self, Error> {
        let original_hash = fingerprint(current)?;
        let candidate_hash = fingerprint(candidate)?;
        let parent = current
            .parent()
            .ok_or("installed binary has no parent directory")?;
        // Exclusive directory creation establishes ownership without deleting or
        // overwriting an earlier update's retained binary or an unrelated file.
        let directory = parent.join(format!(".airc-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let previous = directory.join(
            current
                .file_name()
                .ok_or("installed binary has no filename")?,
        );
        let failed = directory.join("failed-candidate");
        if let Err(error) = move_without_replace(current, &previous) {
            let _ = std::fs::remove_dir(&directory); // Only our still-empty directory.
            return Err(format!(
                "could not displace installed binary {} to {}: {error}; installation was not started and retained paths were not deleted",
                current.display(), previous.display()
            )
            .into());
        }
        Ok(Self {
            current: current.to_owned(),
            previous,
            failed,
            original_hash,
            candidate_hash,
        })
    }

    pub(crate) fn previous(&self) -> &Path {
        &self.previous
    }

    pub(crate) fn rollback(&self) -> Result<(), Error> {
        if fingerprint(&self.previous)? != self.original_hash {
            return Err(format!(
                "owned previous binary changed at {}; refusing an unverifiable rollback",
                self.previous.display()
            )
            .into());
        }
        if self.current.try_exists()? {
            if fingerprint(&self.current)? != self.candidate_hash {
                return Err(format!("{} does not match the prepared candidate; refusing to move unknown contents; previous binary retained at {}", self.current.display(), self.previous.display()).into());
            }
            if self.failed.try_exists()? {
                return Err(format!(
                    "rollback destination {} already exists; no file overwritten",
                    self.failed.display()
                )
                .into());
            }
            move_without_replace(&self.current, &self.failed)?;
        }
        // Renaming restores the original file object, even while the updater
        // still executes from it. Copying over it after this succeeds is invalid
        // on Windows: it attempts to overwrite the updater's own mapped image.
        move_without_replace(&self.previous, &self.current).map_err(|error| {
            format!(
                "could not restore {} to {}: {error}; retained rollback files were not deleted",
                self.previous.display(),
                self.current.display()
            )
        })?;
        if fingerprint(&self.current)? != self.original_hash {
            return Err("restored binary fingerprint differs from the original".into());
        }
        Ok(())
    }
}

/// Move a regular file without the overwrite semantics of std::fs::rename.
/// Destination collisions must preserve both parties, even if created after a
/// prior existence check. All transaction paths share the installation volume.
#[cfg(windows)]
fn move_without_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileW(from: *const u16, to: *const u16) -> i32;
    }
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both immutable buffers are NUL-terminated and live through the call.
    if unsafe { MoveFileW(from.as_ptr(), to.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn move_without_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    // link is atomic and refuses an existing destination. Unlinking the old
    // name preserves mapped executables and never copies over the new name.
    move_by_link(from, to, |path| std::fs::remove_file(path))
}

#[cfg(not(windows))]
fn move_by_link(
    from: &Path,
    to: &Path,
    unlink: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    std::fs::hard_link(from, to).map_err(|error| std::io::Error::new(error.kind(),
        format!("could not create no-overwrite hard link {} -> {}: {error}; this update requires same-volume hard-link support", from.display(), to.display())))?;
    unlink(from).map_err(|error| std::io::Error::new(error.kind(),
        format!("created recovery link {} but could not remove {}: {error}; both names are retained", to.display(), from.display())))
}

fn fingerprint(path: &Path) -> Result<[u8; 32], Error> {
    use std::io::Read;
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(format!("expected a regular binary file at {}", path.display()).into());
    }
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn command(executable: &Path) -> Command {
        let mut command = Command::new(executable);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
    }
    #[test]
    fn rollback_restores_original_while_it_is_running() {
        const MODE: &str = "AIRC_ROLLBACK_MAPPED_FIXTURE";
        if let Some(ready) = std::env::var_os(MODE) {
            std::fs::write(ready, "ready").unwrap();
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let current = temp
            .path()
            .join(format!("airc{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(std::env::current_exe().unwrap(), &current).unwrap();
        let ready = temp.path().join("ready");
        let mut child = OwnedChild(
            command(&current)
                .args([
                    "--exact",
                    "update_rollback::tests::rollback_restores_original_while_it_is_running",
                    "--nocapture",
                ])
                .env(MODE, &ready)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(Instant::now() < deadline, "owned fixture did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        let candidate = temp.path().join("candidate");
        std::fs::write(&candidate, "failed new artifact").unwrap();
        let swap = BinarySwap::displace(&current, &candidate).unwrap();
        std::fs::copy(&candidate, &current).unwrap();
        assert!(child.0.try_wait().unwrap().is_none());
        swap.rollback().unwrap();
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "rollback must not kill its executing caller"
        );
        assert_eq!(fingerprint(&current).unwrap(), swap.original_hash);
        assert_eq!(std::fs::read(&swap.failed).unwrap(), b"failed new artifact");
        assert!(command(&current)
            .arg("--list")
            .output()
            .unwrap()
            .status
            .success());
    }

    #[test]
    fn rollback_preserves_unknown_current_and_owned_destination_collisions() {
        for collision in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let current = temp.path().join("airc");
            let candidate = temp.path().join("candidate");
            std::fs::write(&current, "original").unwrap();
            std::fs::write(&candidate, "new").unwrap();
            let swap = BinarySwap::displace(&current, &candidate).unwrap();
            std::fs::write(&current, if collision { "new" } else { "foreign" }).unwrap();
            if collision {
                std::fs::write(&swap.failed, "foreign").unwrap();
            }
            assert!(swap.rollback().is_err());
            assert_eq!(std::fs::read(&swap.previous).unwrap(), b"original");
            assert_eq!(
                std::fs::read(&current).unwrap(),
                if collision {
                    b"new".as_slice()
                } else {
                    b"foreign".as_slice()
                }
            );
            if collision {
                assert_eq!(std::fs::read(&swap.failed).unwrap(), b"foreign");
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unlink_failure_retains_both_recovery_names() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("original");
        let to = temp.path().join("recovery");
        std::fs::write(&from, "original").unwrap();
        let error = move_by_link(&from, &to, |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert!(error.to_string().contains("both names are retained"));
        assert_eq!(std::fs::read(&from).unwrap(), b"original");
        assert_eq!(std::fs::read(&to).unwrap(), b"original");
    }

    #[test]
    fn rollback_missing_candidate_restores_without_copying() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("airc");
        let candidate = temp.path().join("candidate");
        std::fs::write(&current, "original").unwrap();
        std::fs::write(&candidate, "new").unwrap();
        let swap = BinarySwap::displace(&current, &candidate).unwrap();
        swap.rollback().unwrap();
        assert_eq!(std::fs::read(current).unwrap(), b"original");
    }
}
