//! `airc-rs log ...` handlers for legacy shell log paths.

use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use uuid::Uuid;

const LOCK_STALE: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_SLEEP: Duration = Duration::from_millis(50);
const TAIL_LIMIT: usize = 5_000;

pub fn run_append(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    if input.is_empty() {
        return Ok(());
    }
    append_unique_sig(path, &input)?;
    Ok(())
}

pub fn run_rotate(path: &Path, max_lines: usize, keep_lines: usize) -> Result<(), Box<dyn Error>> {
    rotate_if_needed(path, max_lines, keep_lines)?;
    Ok(())
}

fn append_unique_sig(path: &Path, line: &str) -> Result<AppendOutcome, Box<dyn Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let framed = if line.ends_with('\n') {
        line.to_string()
    } else {
        format!("{line}\n")
    };
    let sig = line_sig(&framed);
    let lock = LogLock::acquire(path)?;
    let outcome = if sig
        .as_deref()
        .is_some_and(|sig| recent_sigs(path, TAIL_LIMIT).contains(&sig.to_string()))
    {
        AppendOutcome::Skipped
    } else {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(framed.as_bytes())?;
        AppendOutcome::Appended
    };
    drop(lock);
    Ok(outcome)
}

fn rotate_if_needed(
    path: &Path,
    max_lines: usize,
    keep_lines: usize,
) -> Result<RotateOutcome, Box<dyn Error>> {
    if max_lines <= keep_lines {
        return Err("max-lines must be greater than keep-lines".into());
    }
    if !path.is_file() {
        return Ok(RotateOutcome::Noop);
    }

    let content = fs::read(path)?;
    let lines = split_lines(&content);
    if lines.len() <= max_lines {
        return Ok(RotateOutcome::Noop);
    }

    let tmp_path = temp_path_for(path);
    {
        let mut tmp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        for line in lines.iter().skip(lines.len() - keep_lines) {
            tmp.write_all(line)?;
        }
    }

    match fs::rename(&tmp_path, path) {
        Ok(()) => Ok(RotateOutcome::Rotated),
        Err(first_error) => {
            if let Err(remove_error) = fs::remove_file(path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(format!(
                    "failed to replace {}: {first_error}; remove failed: {remove_error}",
                    path.display()
                )
                .into());
            }
            if let Err(rename_error) = fs::rename(&tmp_path, path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(format!(
                    "failed to replace {} after remove: {rename_error}",
                    path.display()
                )
                .into());
            }
            Ok(RotateOutcome::Rotated)
        }
    }
}

fn line_sig(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    value
        .get("sig")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn recent_sigs(path: &Path, limit: usize) -> Vec<String> {
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut sigs: Vec<String> = content
        .lines()
        .rev()
        .filter_map(line_sig)
        .take(limit)
        .collect();
    sigs.reverse();
    sigs
}

fn split_lines(content: &[u8]) -> Vec<&[u8]> {
    if content.is_empty() {
        return Vec::new();
    }

    let mut start = 0;
    let mut lines = Vec::new();
    for (index, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&content[start..=index]);
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

fn temp_path_for(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(
        ".airc-log-{}-{}.tmp",
        std::process::id(),
        Uuid::new_v4()
    ))
}

struct LogLock {
    path: PathBuf,
}

impl LogLock {
    fn acquire(path: &Path) -> Result<Self, Box<dyn Error>> {
        let lock_path = path.with_extension(format!(
            "{}lock",
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| format!("{extension}."))
                .unwrap_or_default()
        ));
        let started = SystemTime::now();

        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(Self { path: lock_path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if is_stale_lock(&lock_path) {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                    if started.elapsed().unwrap_or_default() >= LOCK_WAIT {
                        return Err(format!(
                            "timed out waiting for log lock: {}",
                            lock_path.display()
                        )
                        .into());
                    }
                    thread::sleep(LOCK_SLEEP);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for LogLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale_lock(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|elapsed| elapsed > LOCK_STALE)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendOutcome {
    Appended,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotateOutcome {
    Noop,
    Rotated,
}

#[cfg(test)]
mod tests {
    use super::{append_unique_sig, rotate_if_needed, AppendOutcome, RotateOutcome};

    #[test]
    fn append_skips_duplicate_sig_from_recent_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        assert_eq!(
            append_unique_sig(&path, r#"{"sig":"a","msg":"one"}"#).unwrap(),
            AppendOutcome::Appended
        );
        assert_eq!(
            append_unique_sig(&path, r#"{"sig":"a","msg":"duplicate"}"#).unwrap(),
            AppendOutcome::Skipped
        );

        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("one"));
        assert!(!content.contains("duplicate"));
    }

    #[test]
    fn append_without_sig_always_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        append_unique_sig(&path, r#"{"msg":"one"}"#).unwrap();
        append_unique_sig(&path, r#"{"msg":"one"}"#).unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 2);
    }

    #[test]
    fn rotate_keeps_tail_when_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");
        std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

        assert_eq!(
            rotate_if_needed(&path, 3, 2).unwrap(),
            RotateOutcome::Rotated
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "three\nfour\n");
    }

    #[test]
    fn rotate_noops_under_limit_or_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        assert_eq!(rotate_if_needed(&path, 3, 2).unwrap(), RotateOutcome::Noop);
        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(rotate_if_needed(&path, 3, 2).unwrap(), RotateOutcome::Noop);
    }

    #[test]
    fn rotate_rejects_non_headroom_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages.jsonl");

        assert!(rotate_if_needed(&path, 2, 2).is_err());
    }
}
