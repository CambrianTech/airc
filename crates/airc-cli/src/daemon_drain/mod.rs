//! The detached daemon owns its log (card 92cd55d2): its stdout and stderr are a pipe this
//! process drains into [`airc_core::daemon_log::RotatingLog`], so every line it writes, a startup
//! failure or a panic included, lands in one bounded file. A foreground `airc daemon` (no
//! `--log-file`) keeps the terminal's streams unchanged.
//!
//! The one spawn of a detached daemon is [`spawn`]: it names the log, and gives the child that
//! file as its stderr only until the child installs its drain, so a failure before that (an
//! argument the daemon refuses) is still written down.
//!
//! What differs by OS, and only that, is in `os`: how stdout and stderr become the pipe, and how
//! they are let go at exit.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use airc_core::daemon_log::{RotatingLog, BOUND};

#[cfg_attr(unix, path = "unix.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod os;

/// How long exit waits for the drain to write what the pipe still holds. The pipe holds at most
/// one kernel buffer (64 KiB on Linux and macOS), read in milliseconds; the bound exists only so
/// a child that inherited the pipe and outlives the daemon cannot hold its exit open.
const DRAIN_AT_EXIT: Duration = Duration::from_secs(1);

/// The bytes one read takes from the pipe, and the most a line without an end is held before it
/// is written anyway (a writer that never ends a line cannot grow this process's memory).
const READ_CHUNK: usize = 64 << 10;

/// The detached daemon's streams, drained into its log until this is dropped.
pub(crate) struct Drain {
    done: mpsc::Receiver<()>,
    stdio: os::Redirected,
}

/// Make this process's stdout and stderr a pipe drained into the log at `path`.
pub(crate) fn install(path: &Path) -> std::io::Result<Drain> {
    let mut log = RotatingLog::open(path, BOUND)?;
    let (reader, stdio) = os::redirect()?;
    let (finished, done) = mpsc::channel();
    std::thread::Builder::new()
        .name("airc-daemon-log".into())
        .spawn(move || {
            drain(reader, &mut log);
            // The receiver may be gone (exit stopped waiting); nothing is owed then.
            let _ = finished.send(());
        })?;
    Ok(Drain { done, stdio })
}

impl Drop for Drain {
    fn drop(&mut self) {
        // What this process wrote is in the pipe before its ends are let go.
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        self.stdio.release();
        // Bounded: see DRAIN_AT_EXIT. A timeout leaves the rest to the process's exit.
        let _ = self.done.recv_timeout(DRAIN_AT_EXIT);
    }
}

/// Copy the pipe into the log in whole lines until every writer has let it go.
fn drain(mut reader: impl Read, log: &mut RotatingLog) {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&chunk[..n]);
                let whole = match pending.iter().rposition(|&b| b == b'\n') {
                    Some(end) => end + 1,
                    None if pending.len() >= READ_CHUNK => pending.len(),
                    None => continue,
                };
                let rest = pending.split_off(whole);
                // This thread IS the daemon's stderr: a write it cannot make has nowhere to be
                // reported, and the next write tries again.
                let _ = log.write_all(&pending);
                pending = rest;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        let _ = log.write_all(&pending); // as above: nowhere to report a failed write
    }
}

/// Spawn a detached daemon from `command` (built by `airc_lib::daemon_command`, which names its
/// `--log-file`: `airc_lib::daemon_log_path`), with `log` as its stdout and stderr until its
/// drain is installed.
pub(crate) fn spawn(mut command: Command, log: &Path) -> std::io::Result<Child> {
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bootstrap = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(bootstrap.try_clone()?))
        .stderr(Stdio::from(bootstrap));
    crate::commands::spawn_daemon(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a drain that splits a line across a rotation or a read, drops the last
    // line of a writer that exits mid-line, or holds an unended line forever. Lines arrive whole
    // in the log, the tail is written at end of input, and an endless line is written once it
    // fills a chunk.
    #[test]
    fn the_drain_writes_whole_lines_and_the_tail_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("airc-daemon.log");
        let mut log = RotatingLog::open(&path, BOUND).unwrap();
        let input = b"first line\nsecond li"
            .iter()
            .chain(b"ne\nno end".iter())
            .copied()
            .collect::<Vec<u8>>();
        drain(std::io::Cursor::new(input), &mut log);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first line\nsecond line\nno end"
        );

        let endless = vec![b'x'; READ_CHUNK * 2];
        let mut log = RotatingLog::open(&dir.path().join("endless.log"), BOUND).unwrap();
        drain(std::io::Cursor::new(endless), &mut log);
        assert_eq!(
            std::fs::metadata(dir.path().join("endless.log"))
                .unwrap()
                .len(),
            (READ_CHUNK * 2) as u64
        );
    }
}
