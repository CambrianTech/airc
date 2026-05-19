//! Retention + garbage collection for `FsStore`.
//!
//! Two complementary mechanisms:
//!
//! 1. **Age-based** — delete blobs whose last-modified time is older
//!    than `max_age`. Catches "old enough that nothing should still
//!    reference it" — operator policy.
//! 2. **Capacity-based** — when total store bytes exceed `max_bytes`,
//!    delete oldest-first until under the cap. Catches "disk filling
//!    up" — soft-cap.
//!
//! Both are advisory: GC trusts the operator's policy. Reference
//! tracking (which blobs are currently pointed at by live frames /
//! cached state) is a higher layer; GC does NOT consult it. Callers
//! that need stronger safety should set `max_age` long enough that
//! anything still in flight has been delivered + acknowledged.
//!
//! Per #652 Phase 1 airc-blobs item: "GC + retention policy".

use crate::BlobError;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Retention policy — either or both can fire. `None` disables that
/// mechanism. A run that hits both produces deletions from each.
#[derive(Debug, Clone, Default)]
pub struct RetentionPolicy {
    /// Delete blobs whose mtime is older than `max_age`.
    pub max_age: Option<Duration>,
    /// Trigger capacity-based GC when total store bytes exceed this.
    /// Delete oldest-first until the cap is satisfied.
    pub max_bytes: Option<u64>,
}

/// Report of a single GC run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Number of blob files scanned.
    pub scanned: usize,
    /// Number of blob files deleted.
    pub deleted: usize,
    /// Total bytes freed.
    pub bytes_freed: u64,
    /// Total bytes remaining after this run.
    pub bytes_remaining: u64,
}

/// Run garbage collection on the filesystem rooted at `root`.
///
/// Walks every `.blob` file under `root` (one shallow level of
/// fan-out dirs — the `FsStore` layout). Applies age-based deletion
/// first, then capacity-based oldest-first deletion if needed.
///
/// Returns a `GcReport` summarizing what happened. Safe to call
/// concurrently with `FsStore::put` / `get` — race-resistant by
/// design (delete-of-absent is Ok in the FsStore impl).
pub fn run<P: AsRef<Path>>(root: P, policy: &RetentionPolicy) -> Result<GcReport, BlobError> {
    let root = root.as_ref();
    let mut report = GcReport::default();
    let mut all_blobs: Vec<BlobMeta> = Vec::new();

    // Phase 1: walk the FsStore layout, collect (path, size, mtime) for every blob.
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(BlobError::Io(format!("read_dir {root:?}: {e}"))),
    };
    for entry in entries {
        let entry = entry.map_err(|e| BlobError::Io(format!("dir entry: {e}")))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Inner dir contains the blob files for this fan-out bucket.
        let inner =
            fs::read_dir(&path).map_err(|e| BlobError::Io(format!("read_dir {path:?}: {e}")))?;
        for blob_entry in inner {
            let blob_entry = blob_entry.map_err(|e| BlobError::Io(format!("dir entry: {e}")))?;
            let blob_path = blob_entry.path();
            if blob_path.extension().and_then(|s| s.to_str()) != Some("blob") {
                // Skip .tmp.* files (in-flight writes) + anything else
                continue;
            }
            let meta = blob_entry
                .metadata()
                .map_err(|e| BlobError::Io(format!("metadata {blob_path:?}: {e}")))?;
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            all_blobs.push(BlobMeta {
                path: blob_path,
                size: meta.len(),
                mtime,
            });
            report.scanned += 1;
        }
    }

    // Phase 2: age-based deletion.
    if let Some(max_age) = policy.max_age {
        let cutoff = SystemTime::now()
            .checked_sub(max_age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        all_blobs.retain(|b| {
            if b.mtime < cutoff && delete_one(&b.path, &mut report) {
                return false; // Drop from the list — it's gone.
            }
            true
        });
    }

    // Phase 3: capacity-based deletion. Sort by mtime ascending, delete
    // oldest until total under cap. Trust mtime over creation time
    // (creation time isn't portable; mtime is set on each successful put).
    if let Some(max_bytes) = policy.max_bytes {
        let total_remaining: u64 = all_blobs.iter().map(|b| b.size).sum();
        if total_remaining > max_bytes {
            all_blobs.sort_by_key(|b| b.mtime);
            let mut remaining = total_remaining;
            for b in &all_blobs {
                if remaining <= max_bytes {
                    break;
                }
                if delete_one(&b.path, &mut report) {
                    remaining = remaining.saturating_sub(b.size);
                }
            }
            report.bytes_remaining = remaining;
        } else {
            report.bytes_remaining = total_remaining;
        }
    } else {
        report.bytes_remaining = all_blobs.iter().map(|b| b.size).sum();
    }

    Ok(report)
}

struct BlobMeta {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

/// Try to delete one blob; on success, update the report. Returns
/// true if the file was deleted (or was already gone), false on an
/// I/O failure other than NotFound. NotFound is treated as success —
/// race with another caller's delete is a no-op for GC.
fn delete_one(path: &Path, report: &mut GcReport) -> bool {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    match fs::remove_file(path) {
        Ok(()) => {
            report.deleted += 1;
            report.bytes_freed += size;
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentAddressedStore, FsStore};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn fresh_root() -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p =
            std::env::temp_dir().join(format!("airc-blobs-gc-test-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&p);
        p
    }

    /// Set the mtime of `path` to `seconds_ago` seconds in the past.
    /// Test helper — production code never touches mtime directly.
    fn set_mtime_seconds_ago(path: &Path, seconds_ago: u64) {
        let target = SystemTime::now()
            .checked_sub(Duration::from_secs(seconds_ago))
            .expect("subtract from now");
        let file = fs::File::open(path).expect("open");
        file.set_modified(target).expect("set_modified");
    }

    /// What this catches: GC of an empty store is a no-op + reports
    /// zero counts. Boundary case — many GC bugs hide in "what if
    /// there's nothing to scan?"
    #[test]
    fn gc_empty_store_is_noop() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        // Bring root into existence
        let _ = store.put(b"x").expect("put");
        store
            .delete(&crate::ContentHash::from_bytes(b"x"))
            .expect("delete");
        let report = run(&root, &RetentionPolicy::default()).expect("gc");
        assert_eq!(report.deleted, 0);
        assert_eq!(report.bytes_freed, 0);
    }

    /// What this catches: GC of a non-existent root returns a default
    /// report (not an Io error). The store hasn't been used yet —
    /// not a failure state.
    #[test]
    fn gc_missing_root_is_noop() {
        let root = fresh_root();
        let report = run(&root, &RetentionPolicy::default()).expect("gc");
        assert_eq!(report, GcReport::default());
    }

    /// What this catches: age-based deletion removes only blobs older
    /// than max_age. Pins the cutoff math.
    #[test]
    fn gc_age_deletes_only_older_than_cutoff() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let old_hash = store.put(b"old content").expect("put old");
        let new_hash = store.put(b"new content").expect("put new");

        // Mark "old" blob as 1 hour ago, "new" stays fresh
        let old_path = store_path_for(&root, &old_hash);
        let new_path = store_path_for(&root, &new_hash);
        set_mtime_seconds_ago(&old_path, 3600); // 1 hour old

        let policy = RetentionPolicy {
            max_age: Some(Duration::from_secs(60)), // 60s cutoff
            ..Default::default()
        };
        let report = run(&root, &policy).expect("gc");

        assert_eq!(report.scanned, 2);
        assert_eq!(report.deleted, 1, "should delete only the old blob");
        assert!(!old_path.exists(), "old should be deleted");
        assert!(new_path.exists(), "new should remain");
        assert_eq!(report.bytes_freed, "old content".len() as u64);
    }

    /// What this catches: capacity-based GC deletes oldest-first
    /// until under the cap. Pins LRU-style behavior.
    #[test]
    fn gc_capacity_deletes_oldest_first() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let h_old = store.put(b"old____________").expect("put"); // 15 bytes
        let h_mid = store.put(b"mid____________").expect("put"); // 15 bytes
        let h_new = store.put(b"new____________").expect("put"); // 15 bytes

        let p_old = store_path_for(&root, &h_old);
        let p_mid = store_path_for(&root, &h_mid);
        let p_new = store_path_for(&root, &h_new);
        set_mtime_seconds_ago(&p_old, 300);
        set_mtime_seconds_ago(&p_mid, 200);
        set_mtime_seconds_ago(&p_new, 100);

        // Cap at 20 bytes — need to delete old + mid (= 30 bytes freed)
        // to get total down to 15 (just new remains).
        let policy = RetentionPolicy {
            max_bytes: Some(20),
            ..Default::default()
        };
        let report = run(&root, &policy).expect("gc");

        assert_eq!(report.scanned, 3);
        assert_eq!(report.deleted, 2, "should delete two oldest");
        assert!(!p_old.exists(), "oldest deleted");
        assert!(!p_mid.exists(), "second-oldest deleted");
        assert!(p_new.exists(), "newest survives");
        assert_eq!(report.bytes_remaining, 15);
    }

    /// What this catches: capacity GC stops as soon as under-cap;
    /// does NOT delete down to zero.
    #[test]
    fn gc_capacity_stops_when_under_cap() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let _h_old = store.put(b"old____________").expect("put");
        let h_new = store.put(b"new____________").expect("put");
        set_mtime_seconds_ago(&store_path_for(&root, &_h_old), 300);

        // Cap at 20 — total is 30 — delete only the oldest 15 to get under 20
        let policy = RetentionPolicy {
            max_bytes: Some(20),
            ..Default::default()
        };
        let report = run(&root, &policy).expect("gc");

        assert_eq!(report.deleted, 1);
        assert!(store_path_for(&root, &h_new).exists());
        assert_eq!(report.bytes_remaining, 15);
    }

    /// What this catches: no policy → no deletions. The default policy
    /// is "report only" — useful for monitoring without touching disk.
    #[test]
    fn gc_default_policy_is_report_only() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        store.put(b"keep me").expect("put");
        let report = run(&root, &RetentionPolicy::default()).expect("gc");
        assert_eq!(report.scanned, 1);
        assert_eq!(report.deleted, 0);
        assert_eq!(report.bytes_remaining, "keep me".len() as u64);
    }

    /// What this catches: GC ignores .tmp.* files (in-flight writes from
    /// FsStore::put). Without this, a concurrent put + gc race would
    /// briefly count the tmp file then panic trying to delete a file
    /// that the put just renamed.
    #[test]
    fn gc_ignores_tmp_files() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let hash = store.put(b"real blob").expect("put");
        // Manually drop a .tmp sibling that looks like an in-flight write
        let blob_path = store_path_for(&root, &hash);
        let tmp_path = blob_path.with_extension("blob.tmp.99999.0");
        fs::write(&tmp_path, b"tmp content").expect("write tmp");

        let report = run(&root, &RetentionPolicy::default()).expect("gc");
        assert_eq!(report.scanned, 1, ".tmp file should be ignored");
        assert!(tmp_path.exists(), ".tmp file should be untouched");
    }

    /// Helper: compute the FsStore path for a hash without instantiating
    /// the store again. Mirrors the documented layout.
    fn store_path_for(root: &Path, hash: &crate::ContentHash) -> PathBuf {
        let hex = hash.to_hex();
        let (fanout, rest) = hex.split_at(2);
        root.join(fanout).join(format!("{rest}.blob"))
    }
}
