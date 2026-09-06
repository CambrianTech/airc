//! Persistent work-board projection cache (card 1291173d).
//!
//! Every complete work-board read used to replay the room's full
//! transcript from event zero — `airc work board`, `airc work next`,
//! every claim/state mutation guard, every merger tick. The daemon's
//! owner-core resume model already pages strictly-after a cursor, so
//! the complete projection is incrementalizable: snapshot the
//! projection keyed by the last-applied transcript cursor, and on the
//! next read apply only the events that landed after it.
//!
//! Invariants:
//!
//! - **Projection semantics are untouched.** The cache stores the
//!   output of the exact same decode + `apply_windowed` pipeline the
//!   from-scratch rebuild uses; resuming from snapshot `S` at cursor
//!   `c` with events `(c, tip]` is the same fold as replaying
//!   `[0, tip]` — first-write-wins arbitration and close-guards
//!   included.
//! - **Fail loud, rebuild from scratch.** Any anomaly — unreadable or
//!   corrupt cache file, format-version bump, channel or source
//!   mismatch, a cursor the log no longer agrees with (rewound /
//!   wiped store) — logs a warning and falls back to the full
//!   replay. The cache is never trusted over the log; stale state is
//!   never served silently.
//! - **Crash-safe writes.** Snapshots are written to a temp file and
//!   atomically renamed; a torn write is an unreadable cache, which
//!   is just a rebuild.

use std::path::{Path, PathBuf};

use airc_core::{EventId, RoomId, TranscriptCursor};
use airc_work::WorkBoardProjection;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Bump when the persisted shape (or projection semantics feeding it)
/// changes; old snapshots are then discarded and rebuilt.
pub(crate) const WORK_BOARD_CACHE_FORMAT_VERSION: u32 = 1;

/// Subdirectory of the scope home holding one snapshot per room.
const CACHE_DIR: &str = "work-board-cache";

/// The strictly-before-everything resume point: epochs begin at 1 and
/// lamport packs `(epoch, counter)`, so `(0, 0)` precedes every real
/// event on both the daemon and the direct-store read paths.
pub(crate) fn zero_transcript_cursor() -> TranscriptCursor {
    TranscriptCursor {
        lamport: 0,
        event_id: EventId::from_u128(0),
    }
}

/// True iff `left` is strictly before `right` in transcript order —
/// lamport first, event_id tiebreaker. Same total order the stores
/// page by.
pub(crate) fn cursor_strictly_before(left: &TranscriptCursor, right: &TranscriptCursor) -> bool {
    left.lamport < right.lamport
        || (left.lamport == right.lamport && left.event_id.0 < right.event_id.0)
}

/// Which log the snapshot was projected from. A daemon-attached scope
/// reads the owner-core's transcript over IPC; a detached scope reads
/// its local store. They are the same log on a healthy machine, but
/// the cache never assumes that — a snapshot is only resumed on the
/// read path that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkBoardCacheSource {
    Daemon,
    Store,
}

/// A projection of one room's transcript that can be snapshotted and
/// resumed strictly after a cursor. The work board was the first; the
/// wall is the second (2026-09-06: a from-zero wall walk over a 244k-event
/// room took 27–128 s per read on the fleet, inside a 30 s deadline).
/// One cache shape, one resume rule, N projections — the next projection
/// implements this and gets the snapshot for free.
pub(crate) trait CachedProjection: Serialize + DeserializeOwned + Clone {
    /// Subdirectory of the scope home holding one snapshot per room.
    const DIR: &'static str;
    /// Human label for the loud rebuild lines.
    const LABEL: &'static str;
    /// Bump when the persisted shape (or the projection semantics feeding
    /// it) changes; old snapshots are then discarded and rebuilt.
    const VERSION: u32;
}

impl CachedProjection for WorkBoardProjection {
    const DIR: &'static str = CACHE_DIR;
    const LABEL: &'static str = "work board cache";
    const VERSION: u32 = WORK_BOARD_CACHE_FORMAT_VERSION;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProjectionCache<P> {
    pub version: u32,
    pub channel: RoomId,
    pub source: WorkBoardCacheSource,
    /// Cursor of the newest transcript event folded into `projection`
    /// (a projected event or not) — the strictly-after resume point.
    pub cursor: TranscriptCursor,
    pub projection: P,
}

impl<P: CachedProjection> ProjectionCache<P> {
    pub fn path(home: &Path, channel: RoomId) -> PathBuf {
        home.join(P::DIR).join(format!("{channel}.json"))
    }

    /// Load the snapshot for `channel`, or `None` when the caller must
    /// rebuild from scratch. A missing file is the normal cold start
    /// (silent); anything else — unreadable file, corrupt JSON,
    /// version/channel/source mismatch — is loud, because it means a
    /// snapshot existed and is being discarded.
    pub fn load(home: &Path, channel: RoomId, source: WorkBoardCacheSource) -> Option<Self> {
        let path = Self::path(home, channel);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                eprintln!(
                    "{}: failed to read {} ({error}) — rebuilding projection from scratch",
                    P::LABEL,
                    path.display()
                );
                return None;
            }
        };
        let cache: Self = match serde_json::from_slice(&bytes) {
            Ok(cache) => cache,
            Err(error) => {
                eprintln!(
                    "{}: corrupt snapshot {} ({error}) — rebuilding projection from scratch",
                    P::LABEL,
                    path.display()
                );
                return None;
            }
        };
        if cache.version != P::VERSION {
            eprintln!(
                "{}: snapshot {} has format v{} (current v{}) — rebuilding projection from scratch",
                P::LABEL,
                path.display(),
                cache.version,
                P::VERSION
            );
            return None;
        }
        if cache.channel != channel {
            eprintln!(
                "{}: snapshot {} is for channel {} (expected {channel}) — rebuilding projection from scratch",
                P::LABEL,
                path.display(),
                cache.channel
            );
            return None;
        }
        if cache.source != source {
            eprintln!(
                "{}: snapshot {} was projected from the {:?} log but this read uses {:?} — rebuilding projection from scratch",
                P::LABEL,
                path.display(),
                cache.source,
                source
            );
            return None;
        }
        Some(cache)
    }

    /// Persist atomically (temp file + rename). Failure to persist is
    /// loud but non-fatal: the projection just returned to the caller
    /// is correct either way; only the next read's fast path is lost.
    pub fn save(&self, home: &Path) {
        let path = Self::path(home, self.channel);
        if let Err(error) = self.try_save(&path) {
            eprintln!(
                "{}: failed to persist snapshot {} ({error}) — next read rebuilds from scratch",
                P::LABEL,
                path.display()
            );
        }
    }

    fn try_save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::other("cache path has no parent directory"))?;
        std::fs::create_dir_all(dir)?;
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        // Temp name unique per ATTEMPT, not just per process: an embedded
        // consumer (continuum core) runs many board reads concurrently in
        // ONE process, and a pid-only suffix made concurrent saves share a
        // tmp file — the first rename consumed it, the second ENOENT'd, so
        // the snapshot never persisted and every subsequent read full-
        // replayed the room (observed live 2026-07-13: paired failure
        // lines per persona in the continuum core log).
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp.{}.{seq}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(channel: RoomId) -> ProjectionCache<WorkBoardProjection> {
        ProjectionCache {
            version: WORK_BOARD_CACHE_FORMAT_VERSION,
            channel,
            source: WorkBoardCacheSource::Daemon,
            cursor: TranscriptCursor {
                lamport: 42,
                event_id: EventId::from_u128(7),
            },
            projection: WorkBoardProjection::new(),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let home = tempfile::tempdir().expect("tempdir");
        let channel = RoomId::from_u128(1);
        let cache = sample(channel);
        cache.save(home.path());
        let loaded = ProjectionCache::<WorkBoardProjection>::load(
            home.path(),
            channel,
            WorkBoardCacheSource::Daemon,
        )
        .expect("snapshot loads back");
        assert_eq!(loaded, cache);
    }

    // what this catches: concurrent saves to the SAME snapshot path must
    // all persist (continuum #154 tail, live 2026-07-13). The tmp file was
    // named per-process only, so two threads saving one persona's cache
    // shared a tmp: the first rename consumed it, the second ENOENT'd, and
    // the cache never engaged — every board read full-replayed the room.
    #[test]
    fn concurrent_saves_to_one_path_all_persist() {
        let home = tempfile::tempdir().expect("tempdir");
        let channel = RoomId::from_u128(9);
        let results: Vec<std::io::Result<()>> = std::thread::scope(|scope| {
            (0..8)
                .map(|_| {
                    let home = home.path();
                    scope.spawn(move || {
                        let cache = sample(channel);
                        cache.try_save(&ProjectionCache::<WorkBoardProjection>::path(home, channel))
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().expect("thread"))
                .collect()
        });
        for r in results {
            r.expect("every concurrent save must persist — shared tmp names race");
        }
        assert!(ProjectionCache::<WorkBoardProjection>::load(
            home.path(),
            channel,
            WorkBoardCacheSource::Daemon
        )
        .is_some());
    }

    #[test]
    fn missing_file_is_silent_cold_start() {
        let home = tempfile::tempdir().expect("tempdir");
        assert!(ProjectionCache::<WorkBoardProjection>::load(
            home.path(),
            RoomId::from_u128(1),
            WorkBoardCacheSource::Daemon
        )
        .is_none());
    }

    #[test]
    fn corrupt_json_is_discarded() {
        let home = tempfile::tempdir().expect("tempdir");
        let channel = RoomId::from_u128(1);
        let path = ProjectionCache::<WorkBoardProjection>::path(home.path(), channel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{ not json").expect("write garbage");
        assert!(ProjectionCache::<WorkBoardProjection>::load(
            home.path(),
            channel,
            WorkBoardCacheSource::Daemon
        )
        .is_none());
    }

    #[test]
    fn version_mismatch_is_discarded() {
        let home = tempfile::tempdir().expect("tempdir");
        let channel = RoomId::from_u128(1);
        let mut cache = sample(channel);
        cache.version = WORK_BOARD_CACHE_FORMAT_VERSION + 1;
        cache.save(home.path());
        assert!(ProjectionCache::<WorkBoardProjection>::load(
            home.path(),
            channel,
            WorkBoardCacheSource::Daemon
        )
        .is_none());
    }

    #[test]
    fn channel_mismatch_is_discarded() {
        let home = tempfile::tempdir().expect("tempdir");
        let written = RoomId::from_u128(1);
        let cache = sample(written);
        // Force the file under a DIFFERENT channel's name.
        let path = ProjectionCache::<WorkBoardProjection>::path(home.path(), RoomId::from_u128(2));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, serde_json::to_vec(&cache).expect("encode")).expect("write");
        assert!(ProjectionCache::<WorkBoardProjection>::load(
            home.path(),
            RoomId::from_u128(2),
            WorkBoardCacheSource::Daemon
        )
        .is_none());
    }

    #[test]
    fn source_mismatch_is_discarded() {
        let home = tempfile::tempdir().expect("tempdir");
        let channel = RoomId::from_u128(1);
        sample(channel).save(home.path());
        assert!(ProjectionCache::<WorkBoardProjection>::load(
            home.path(),
            channel,
            WorkBoardCacheSource::Store
        )
        .is_none());
    }

    #[test]
    fn cursor_order_is_lamport_then_event_id() {
        let a = TranscriptCursor {
            lamport: 1,
            event_id: EventId::from_u128(9),
        };
        let b = TranscriptCursor {
            lamport: 2,
            event_id: EventId::from_u128(1),
        };
        let b_tie = TranscriptCursor {
            lamport: 2,
            event_id: EventId::from_u128(2),
        };
        assert!(cursor_strictly_before(&a, &b));
        assert!(cursor_strictly_before(&b, &b_tie));
        assert!(!cursor_strictly_before(&b, &b));
        assert!(!cursor_strictly_before(&b, &a));
    }
}
