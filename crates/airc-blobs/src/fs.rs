//! Filesystem-backed `ContentAddressedStore`.
//!
//! Stores each blob at `<root>/<aa>/<bbcc..>.blob` where `aa` is the
//! first two hex chars of the sha256 and `bbcc..` is the remaining 62.
//! The two-char fan-out keeps directory inode counts manageable for
//! large stores (256 leaf dirs at the top level).
//!
//! ## Atomicity
//!
//! Writes go to a `<final>.tmp.<pid>.<rand>` sibling first, then
//! `rename` atomically into place. A crash mid-write leaves the
//! `.tmp` sibling — not the final path — so readers never see a
//! partial blob.
//!
//! ## Verification
//!
//! `put` verifies the bytes hash to the claimed value (not just
//! trusts the caller). `get` re-verifies on every read (paranoid
//! mode toggleable in a follow-up); a corrupted on-disk blob
//! surfaces as `BlobError::HashMismatch` rather than silently
//! returning bad bytes.

use crate::{BlobError, ContentAddressedStore, ContentHash};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

/// Filesystem-backed content-addressed store rooted at a directory.
///
/// Path layout: `<root>/<aa>/<bbcc..>.blob` — sha256 first 2 hex chars
/// for fan-out, remaining 62 for filename. Standard `sha256sum`-style
/// CDN layout.
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    /// Construct a store rooted at `root`. The directory is created if
    /// it doesn't exist. Subsequent operations create per-blob fan-out
    /// dirs lazily.
    pub fn new<P: AsRef<Path>>(root: P) -> Result<Self, BlobError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| BlobError::Io(format!("create_dir_all {root:?}: {e}")))?;
        Ok(Self { root })
    }

    /// Compute the on-disk path for a hash. Pure — no I/O.
    fn path_for(&self, hash: &ContentHash) -> PathBuf {
        let hex = hash.to_hex();
        // hex is always 64 chars (sha256); slicing is safe
        let (fanout, rest) = hex.split_at(2);
        self.root.join(fanout).join(format!("{rest}.blob"))
    }
}

impl ContentAddressedStore for FsStore {
    fn put(&self, bytes: &[u8]) -> Result<ContentHash, BlobError> {
        let hash = ContentHash::from_bytes(bytes);
        let final_path = self.path_for(&hash);

        // Idempotent: if already present, return the hash without rewriting.
        // Saves an IO round-trip for duplicates and avoids racing with
        // a concurrent writer.
        if final_path.exists() {
            return Ok(hash);
        }

        // Ensure fan-out dir exists.
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                BlobError::Io(format!("create_dir_all {parent:?}: {e}"))
            })?;
        }

        // Write to a sibling .tmp file then rename for atomicity.
        // pid + nanos = good-enough uniqueness for concurrent writers on
        // the same host; collision would require two writers at the same
        // nanosecond from the same pid — practically impossible.
        let tmp_path = final_path.with_extension(format!(
            "blob.tmp.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        {
            let mut file = fs::File::create(&tmp_path)
                .map_err(|e| BlobError::Io(format!("create {tmp_path:?}: {e}")))?;
            file.write_all(bytes)
                .map_err(|e| BlobError::Io(format!("write {tmp_path:?}: {e}")))?;
            file.sync_all()
                .map_err(|e| BlobError::Io(format!("fsync {tmp_path:?}: {e}")))?;
        }

        fs::rename(&tmp_path, &final_path).map_err(|e| {
            // Best-effort cleanup of the orphan .tmp on rename failure;
            // ignore the cleanup result since the rename error is the
            // real signal.
            let _ = fs::remove_file(&tmp_path);
            BlobError::Io(format!("rename {tmp_path:?} -> {final_path:?}: {e}"))
        })?;

        Ok(hash)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, BlobError> {
        let path = self.path_for(hash);
        let mut file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(BlobError::NotFound { hash: hash.clone() });
            }
            Err(e) => return Err(BlobError::Io(format!("open {path:?}: {e}"))),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| BlobError::Io(format!("read {path:?}: {e}")))?;

        // Re-verify the on-disk content hashes to the claimed hash.
        // Disk bitrot or external tampering produces typed
        // HashMismatch rather than silent bad bytes.
        let actual = ContentHash::from_bytes(&bytes);
        if actual != *hash {
            return Err(BlobError::HashMismatch {
                expected: hash.clone(),
                actual,
            });
        }
        Ok(bytes)
    }

    fn exists(&self, hash: &ContentHash) -> Result<bool, BlobError> {
        Ok(self.path_for(hash).exists())
    }

    fn delete(&self, hash: &ContentHash) -> Result<(), BlobError> {
        let path = self.path_for(hash);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(BlobError::Io(format!("remove {path:?}: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Each test gets a fresh tempdir under `/tmp/airc-blobs-test-<pid>-<n>/`.
    /// Process id + counter scopes the dir per-test-run so parallel test
    /// invocations don't collide.
    fn fresh_root() -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "airc-blobs-test-{}-{}",
            std::process::id(),
            n
        ));
        // Remove any leftover from a prior crashed test.
        let _ = fs::remove_dir_all(&p);
        p
    }

    /// What this catches: `new` creates the root directory if it
    /// doesn't exist. Saves operators a manual `mkdir` before first
    /// use.
    #[test]
    fn new_creates_root_dir() {
        let root = fresh_root();
        assert!(!root.exists());
        let _store = FsStore::new(&root).expect("new should succeed");
        assert!(root.exists() && root.is_dir());
    }

    /// What this catches: put → get round-trip via the filesystem.
    /// The smoke proof that the trait impl works end-to-end.
    #[test]
    fn put_get_round_trip() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let hash = store.put(b"hello world").expect("put");
        let bytes = store.get(&hash).expect("get");
        assert_eq!(bytes, b"hello world");
    }

    /// What this catches: `path_for` produces the documented
    /// `<root>/<aa>/<bbcc..>.blob` layout. Pin the on-disk layout so
    /// `find . -name '*.blob'` workflows + external tools continue
    /// to work.
    #[test]
    fn path_for_uses_two_char_fanout() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let hash = ContentHash::from_bytes(b"hello world");
        let path = store.path_for(&hash);
        // sha256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
        assert!(path.starts_with(&root));
        let stripped = path.strip_prefix(&root).unwrap();
        assert_eq!(
            stripped,
            Path::new("b9/4d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9.blob")
        );
    }

    /// What this catches: identical puts are idempotent — second put
    /// of the same bytes is a fast no-op (returns same hash, doesn't
    /// rewrite the file). Avoids unnecessary disk IO for dedupe.
    #[test]
    fn put_is_idempotent_for_identical_bytes() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let bytes = b"dedupe me";
        let h1 = store.put(bytes).expect("first put");
        let h2 = store.put(bytes).expect("second put");
        assert_eq!(h1, h2);
        assert_eq!(store.get(&h1).unwrap(), bytes);
    }

    /// What this catches: `get` of an absent hash returns `NotFound`,
    /// not a generic Io error. Callers need to distinguish "ask a
    /// peer" (NotFound) from "disk is on fire" (Io).
    #[test]
    fn get_missing_returns_not_found() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let absent = ContentHash::from_bytes(b"never_stored");
        let result = store.get(&absent);
        assert!(
            matches!(result, Err(BlobError::NotFound { .. })),
            "expected NotFound, got {result:?}"
        );
    }

    /// What this catches: `exists` is true for stored, false for
    /// absent. The fast path that backends override for cheap
    /// presence checks; FsStore overrides to skip the file read.
    #[test]
    fn exists_reflects_storage_state() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let absent = ContentHash::from_bytes(b"absent");
        assert!(!store.exists(&absent).unwrap());
        let stored = store.put(b"present").expect("put");
        assert!(store.exists(&stored).unwrap());
    }

    /// What this catches: delete removes the file from disk +
    /// subsequent `exists` reports false. Pure deletion, no
    /// soft-delete.
    #[test]
    fn delete_removes_blob() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let hash = store.put(b"delete me").expect("put");
        assert!(store.exists(&hash).unwrap());
        store.delete(&hash).expect("delete");
        assert!(!store.exists(&hash).unwrap());
    }

    /// What this catches: delete is idempotent — deleting an absent
    /// hash returns Ok. Callers don't need a "check before delete"
    /// guard pattern.
    #[test]
    fn delete_absent_is_idempotent() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let absent = ContentHash::from_bytes(b"never_stored");
        store.delete(&absent).expect("delete of absent should be Ok");
        // Double-delete also fine
        let hash = store.put(b"x").expect("put");
        store.delete(&hash).expect("first delete");
        store.delete(&hash).expect("second delete should also be Ok");
    }

    /// What this catches: `get` re-verifies on-disk bytes against the
    /// claimed hash. Tampering or bitrot surfaces as typed
    /// HashMismatch rather than silent bad bytes.
    #[test]
    fn get_detects_corruption() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let hash = store.put(b"original content").expect("put");
        // Tamper directly on the filesystem
        let path = store.path_for(&hash);
        fs::write(&path, b"tampered!").expect("write tampered bytes");
        let result = store.get(&hash);
        assert!(
            matches!(result, Err(BlobError::HashMismatch { .. })),
            "expected HashMismatch, got {result:?}"
        );
    }

    /// What this catches: empty input round-trips correctly. Boundary
    /// case some impls get wrong (file-size-zero file is a valid blob
    /// — sha256 of empty bytes is a real hash).
    #[test]
    fn empty_bytes_round_trip() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let hash = store.put(b"").expect("put empty");
        let bytes = store.get(&hash).expect("get empty");
        assert_eq!(bytes, b"");
    }

    /// What this catches: large-ish blob (1 MiB) round-trips without
    /// corruption. Catches accidental partial writes or read truncation.
    #[test]
    fn large_blob_round_trip() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        let bytes: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
        let hash = store.put(&bytes).expect("put 1MiB");
        let read_back = store.get(&hash).expect("get 1MiB");
        assert_eq!(read_back, bytes);
    }

    /// What this catches: putting a SECOND distinct blob into the same
    /// fan-out bucket (same first 2 hex chars) doesn't clobber the
    /// first. Important for the hash space — 256 buckets, many blobs
    /// per bucket expected at scale.
    #[test]
    fn two_blobs_in_same_fanout_bucket_coexist() {
        let root = fresh_root();
        let store = FsStore::new(&root).expect("new");
        // These two values happen to both hash starting with different
        // first-two-chars, so we manufacture the collision via a deterministic
        // search: hash inputs until we find two that share the first 2 hex
        // chars.
        let mut blobs = Vec::new();
        let mut by_prefix: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        for i in 0u64..10000 {
            let bytes = format!("blob-{i}").into_bytes();
            let prefix = ContentHash::from_bytes(&bytes).to_hex()[..2].to_string();
            if let Some(existing) = by_prefix.get(&prefix) {
                blobs.push(existing.clone());
                blobs.push(bytes);
                break;
            }
            by_prefix.insert(prefix, bytes);
        }
        assert_eq!(blobs.len(), 2, "should find two blobs sharing a fan-out prefix within 10000 iterations");

        let h1 = store.put(&blobs[0]).expect("put 1");
        let h2 = store.put(&blobs[1]).expect("put 2");
        assert_ne!(h1, h2, "different bytes must hash differently");
        assert_eq!(store.get(&h1).unwrap(), blobs[0]);
        assert_eq!(store.get(&h2).unwrap(), blobs[1]);
    }
}
