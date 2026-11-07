//! The detached daemon's log: one file under the daemon's own home, bounded by size, newest bytes
//! kept (card 92cd55d2).
//!
//! The IntelMac, 2026-11-02: `<repo>/.airc/airc-daemon.log` held 740 MB. Every spawn opened it for
//! append and handed it to the daemon as stdout and stderr; nothing ever rotated it. The daemon now
//! owns it: its stdout and stderr drain into a [`RotatingLog`] (the drain is the binary's), and the
//! file lives under the daemon's home, where continuum's `airc-daemon-log` disk class already looks
//! for it and its one rotated generation.
//!
//! Rotation renames the live file to `.1`. Where the rename is refused (Windows, a reader holding
//! the file without delete sharing), the live file is copied to `.1` and truncated in place:
//! nothing written is lost, and the bound still holds.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The daemon log's file name under the daemon's home.
pub const FILE: &str = "airc-daemon.log";

/// The detached daemon's log under its home.
pub fn path(daemon_home: &Path) -> PathBuf {
    daemon_home.join(FILE)
}

/// One generation's size, and how many rotated generations stay beside the live file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    pub max_bytes: u64,
    pub keep: usize,
}

/// The daemon log's bound. 8 MiB a generation: continuum's size for its own process logs, a boot
/// banner plus a fault trace (continuum `capped_appender::MAX_LOG_BYTES`). One rotated generation:
/// the layout continuum's disk class for this file already tracks (`airc-daemon.log` and `.1`).
pub const BOUND: Bound = Bound {
    max_bytes: 8 << 20,
    keep: 1,
};

/// An append-only file that cannot outgrow `bound.max_bytes * (bound.keep + 1)`.
pub struct RotatingLog {
    path: PathBuf,
    file: File,
    written: u64,
    bound: Bound,
}

impl RotatingLog {
    /// Append to `path`. A file already past the bound is cut to its newest whole lines first, and
    /// its size counts, so a restart never grants a fresh generation.
    pub fn open(path: &Path, bound: Bound) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        keep_tail_of_oversized(path, bound.max_bytes)?;
        let file = append(path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            file,
            written,
            bound,
        })
    }

    /// Write `bytes` whole, rotating first when they would cross the bound. A rotation that fails
    /// leaves the live file open and is asked again by the next write past the bound: a log that
    /// stops the daemon is worse than one that ran over by a write.
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.written > 0 && self.written + bytes.len() as u64 > self.bound.max_bytes {
            if let Ok(file) = self.rotate() {
                self.file = file;
                self.written = 0;
            }
        }
        self.file.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    /// `<path>.n`.
    pub fn generation(&self, n: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    /// Shift every generation up one and start a fresh live file; returns it.
    fn rotate(&mut self) -> io::Result<File> {
        if self.bound.keep == 0 {
            return truncate(&self.path);
        }
        remove_if_present(&self.generation(self.bound.keep))?;
        for n in (2..=self.bound.keep).rev() {
            rename_if_present(&self.generation(n - 1), &self.generation(n))?;
        }
        match std::fs::rename(&self.path, self.generation(1)) {
            Ok(()) => append(&self.path),
            // Refused while a reader holds the live file (Windows without delete sharing): the
            // records move by copy, and the live file is cut in place under the same handle.
            Err(_) => {
                std::fs::copy(&self.path, self.generation(1))?;
                truncate(&self.path)
            }
        }
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn truncate(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(0)?;
    append(path)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn rename_if_present(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Cut a file past `max_bytes` to its newest `max_bytes`, from its first line start.
fn keep_tail_of_oversized(path: &Path, max_bytes: u64) -> io::Result<()> {
    let len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if len <= max_bytes {
        return Ok(());
    }
    let mut tail = Vec::new();
    {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(len - max_bytes))?;
        file.take(max_bytes).read_to_end(&mut tail)?;
    }
    let start = tail
        .iter()
        .position(|&b| b == b'\n')
        .map_or(tail.len(), |i| i + 1);
    let mut file = truncate(path)?;
    file.write_all(&tail[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on_disk(dir: &Path) -> u64 {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum()
    }

    // what this catches (the IntelMac's 740 MB daemon log, 2026-11-02): a writer that never
    // stops growing its files past `max_bytes * (keep + 1)`, and a rotation that keeps the
    // oldest bytes live instead of the newest.
    #[test]
    fn a_runaway_writer_is_bounded_and_the_live_file_holds_the_newest_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let bound = Bound {
            max_bytes: 1024,
            keep: 1,
        };
        let mut log = RotatingLog::open(&dir.path().join(FILE), bound).unwrap();
        for i in 0..500 {
            log.write_all(format!("line {i:04} {}\n", "x".repeat(80)).as_bytes())
                .unwrap();
        }
        assert!(on_disk(dir.path()) <= 2 * 1024, "{}", on_disk(dir.path()));
        let live = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(live.contains("line 0499"), "the newest line is live");
        assert!(
            dir.path().join(format!("{FILE}.1")).exists()
                && !dir.path().join(format!("{FILE}.2")).exists()
        );
    }

    // what this catches: a file left oversized by an unbounded writer (the old spawn) adopted
    // whole, so the bound would not hold until it rotated; and a restart that resets the count.
    #[test]
    fn an_oversized_file_keeps_its_newest_whole_lines_and_its_size_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let lines: String = (0..200).map(|i| format!("{{\"n\":{i}}}\n")).collect();
        std::fs::write(&path, &lines).unwrap();
        let mut log = RotatingLog::open(
            &path,
            Bound {
                max_bytes: 256,
                keep: 1,
            },
        )
        .unwrap();
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(
            kept.len() <= 256 && kept.ends_with("{\"n\":199}\n"),
            "{kept}"
        );
        assert!(
            kept.lines()
                .all(|l| l.starts_with("{\"n\":") && l.ends_with('}')),
            "{kept}"
        );
        log.write_all(&vec![b'z'; 256 - kept.len() + 1]).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(format!("{FILE}.1"))).unwrap(),
            kept,
            "the adopted size counted"
        );
    }

    // what this catches (Codex on card 92cd55d2): a rotation refused by a reader that holds the
    // live file without delete sharing, which either lost the records or let the live file grow
    // without bound. The records move to `.1` by copy and the live file is cut in place.
    #[cfg(windows)]
    #[test]
    fn a_rotation_refused_by_a_reader_loses_nothing_and_stays_bounded() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let bound = Bound {
            max_bytes: 1024,
            keep: 1,
        };
        let mut log = RotatingLog::open(&path, bound).unwrap();
        log.write_all(&vec![b'a'; 1000]).unwrap();
        // FILE_SHARE_READ | FILE_SHARE_WRITE, no FILE_SHARE_DELETE: a rename of the file fails.
        let _reader = OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&path)
            .unwrap();
        assert!(
            std::fs::rename(&path, dir.path().join("probe")).is_err(),
            "test: the reader denies the rename"
        );
        // One refused rotation: the first `b` crosses the bound.
        for _ in 0..5 {
            log.write_all(&[b'b'; 100]).unwrap();
        }
        assert_eq!(
            std::fs::read(dir.path().join(format!("{FILE}.1"))).unwrap(),
            vec![b'a'; 1000],
            "the refused rotation moved every record to .1"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            vec![b'b'; 500],
            "the live file was cut in place"
        );
        // Still refused on every later rotation, and still bounded.
        for _ in 0..50 {
            log.write_all(&[b'b'; 100]).unwrap();
        }
        assert!(on_disk(dir.path()) <= 2 * 1024, "{}", on_disk(dir.path()));
    }
}
