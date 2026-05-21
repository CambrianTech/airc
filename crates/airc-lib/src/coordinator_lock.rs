use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::coordinator::{CoordinatorError, RefreshLockOutcome};
use crate::fs_permissions;

const REFRESH_LOCK_MAX_RETRIES: u32 = 8;
const TRANSITION_PAUSE: Duration = Duration::from_millis(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LockFileContents {
    pub held_at_ms: u64,
    pub holder_pid: u32,
}

pub(crate) struct RefreshLock {
    main: LockFile,
    takeover: LockFile,
    refresh_interval_ms: u64,
    now_ms: u64,
    payload: String,
}

impl RefreshLock {
    pub(crate) fn new(
        main_path: PathBuf,
        takeover_path: PathBuf,
        refresh_interval_ms: u64,
        now_ms: u64,
        holder_pid: u32,
    ) -> Result<Self, CoordinatorError> {
        let contents = LockFileContents {
            held_at_ms: now_ms,
            holder_pid,
        };
        Ok(Self {
            main: LockFile::new(main_path),
            takeover: LockFile::new(takeover_path),
            refresh_interval_ms,
            now_ms,
            payload: serde_json::to_string_pretty(&contents)?,
        })
    }

    pub(crate) fn acquire(&self) -> Result<RefreshLockOutcome, CoordinatorError> {
        for _ in 0..REFRESH_LOCK_MAX_RETRIES {
            if let Some(outcome) = self.observe_takeover()? {
                return Ok(outcome);
            }

            match self.main.create(&self.payload)? {
                CreateResult::Created => return Ok(RefreshLockOutcome::Acquired),
                CreateResult::Unavailable => self.pause(),
                CreateResult::Exists => {
                    match self.main.state(self.now_ms, self.refresh_interval_ms)? {
                        LockState::Fresh { held_at_ms } => {
                            return Ok(RefreshLockOutcome::HeldFresh { held_at_ms });
                        }
                        LockState::Stale => return self.take_over_stale_main(),
                        LockState::Missing | LockState::Transition => self.pause(),
                    }
                }
            }
        }

        Ok(RefreshLockOutcome::HeldFresh {
            held_at_ms: self.now_ms,
        })
    }

    fn observe_takeover(&self) -> Result<Option<RefreshLockOutcome>, CoordinatorError> {
        match self.takeover.state(self.now_ms, self.refresh_interval_ms)? {
            LockState::Fresh { held_at_ms } => {
                Ok(Some(RefreshLockOutcome::HeldFresh { held_at_ms }))
            }
            LockState::Stale => {
                self.takeover.remove();
                Ok(None)
            }
            LockState::Missing | LockState::Transition => Ok(None),
        }
    }

    fn take_over_stale_main(&self) -> Result<RefreshLockOutcome, CoordinatorError> {
        match self.takeover.create(&self.payload)? {
            CreateResult::Created => {
                let outcome = self.replace_main_while_holding_takeover();
                self.takeover.remove();
                outcome
            }
            CreateResult::Exists | CreateResult::Unavailable => Ok(RefreshLockOutcome::HeldFresh {
                held_at_ms: self.now_ms,
            }),
        }
    }

    fn replace_main_while_holding_takeover(&self) -> Result<RefreshLockOutcome, CoordinatorError> {
        for _ in 0..REFRESH_LOCK_MAX_RETRIES {
            match self.main.state(self.now_ms, self.refresh_interval_ms)? {
                LockState::Fresh { held_at_ms } => {
                    return Ok(RefreshLockOutcome::HeldFresh { held_at_ms });
                }
                LockState::Stale | LockState::Transition => self.main.remove(),
                LockState::Missing => {}
            }

            match self.main.create(&self.payload)? {
                CreateResult::Created => return Ok(RefreshLockOutcome::Acquired),
                CreateResult::Exists | CreateResult::Unavailable => self.pause(),
            }
        }

        Ok(RefreshLockOutcome::HeldFresh {
            held_at_ms: self.now_ms,
        })
    }

    fn pause(&self) {
        std::thread::sleep(TRANSITION_PAUSE);
    }
}

struct LockFile {
    path: PathBuf,
}

impl LockFile {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn create(&self, payload: &str) -> Result<CreateResult, CoordinatorError> {
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                file.write_all(payload.as_bytes())?;
                file.sync_all()?;
                fs_permissions::set_owner_only(&self.path)?;
                Ok(CreateResult::Created)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(CreateResult::Exists)
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Ok(CreateResult::Unavailable)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn state(&self, now_ms: u64, fresh_for_ms: u64) -> Result<LockState, CoordinatorError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => match serde_json::from_str::<LockFileContents>(&text) {
                Ok(existing) if now_ms.saturating_sub(existing.held_at_ms) < fresh_for_ms => {
                    Ok(LockState::Fresh {
                        held_at_ms: existing.held_at_ms,
                    })
                }
                Ok(_) => Ok(LockState::Stale),
                Err(_) => Ok(LockState::Transition),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(LockState::Missing),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Ok(LockState::Transition)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn remove(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

enum CreateResult {
    Created,
    Exists,
    Unavailable,
}

enum LockState {
    Missing,
    Fresh { held_at_ms: u64 },
    Stale,
    Transition,
}
