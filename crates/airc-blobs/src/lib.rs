//! Content-addressed blob storage for airc.
//!
//! The airc wire protocol carries small frames over the transport. Large
//! bodies (audio segments, images, attached files, model weights) DO NOT
//! travel inside frames; they live in a content-addressed store and the
//! frame carries a `MediaRef` pointer (sha256 + size + optional MIME).
//!
//! This crate is the trait + types layer. Phase 1 follow-up PRs ship the
//! concrete backends:
//!   1. Filesystem-backed `FsStore` (sha256-addressed paths)
//!   2. Chunked upload/download with resume
//!   3. At-rest encryption with per-room key
//!   4. GC + retention policy
//!
//! ## Design notes (per airc-rust design doc #651)
//!
//! - **Content-addressed.** Identity = SHA-256 of the bytes. Two identical
//!   uploads dedupe naturally. Tamper-evident: anyone can verify the
//!   stored bytes match the claimed hash.
//! - **Backend-pluggable.** `ContentAddressedStore` is the trait; concrete
//!   backends (fs, s3-compatible, ipfs) plug in without touching callers.
//! - **No silent failure paths.** All `Result` variants are typed in
//!   `BlobError`; backends must surface IO / corruption / not-found
//!   distinctly so callers can act (retry vs re-request from peer vs
//!   permanent drop).
//! - **Encryption is OPTIONAL at this layer.** Per-room key encryption is
//!   a follow-up item; the trait shape lets a future `EncryptedStore`
//!   wrap any `ContentAddressedStore` without trait changes.

use serde::{Deserialize, Serialize};

pub mod fs;
pub mod gc;
pub mod hash;

pub use fs::FsStore;
pub use gc::{run as gc_run, GcReport, RetentionPolicy};
pub use hash::ContentHash;

// ─── Errors ───────────────────────────────────────────────────────────

/// All blob-store failures are typed. Callers distinguish "not here, ask
/// a peer" (NotFound) from "we have it but it's corrupted" (HashMismatch)
/// from "the disk is on fire" (Io). No silent fallthrough.
#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    /// The requested content hash is not in the store. Caller may
    /// re-request from a peer or accept that the content is unavailable.
    #[error("blob not found: {hash}")]
    NotFound { hash: ContentHash },

    /// The stored bytes do not hash to the claimed `ContentHash`. The
    /// store is corrupted at this address; caller should treat the
    /// content as missing AND escalate (delete the bad copy, re-request,
    /// log the corruption event).
    #[error("blob hash mismatch at {expected}: stored bytes hash to {actual}")]
    HashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },

    /// Storage capacity exceeded. Backend-specific (filesystem free-space,
    /// quota, etc.). Caller decides whether to retry after GC or refuse
    /// the put.
    #[error("storage capacity exceeded: need {needed_bytes} bytes")]
    CapacityExceeded { needed_bytes: u64 },

    /// Underlying I/O error. Backend-agnostic carrier for the
    /// std::io::Error / network error that prevented the operation.
    /// Caller surfaces the inner message; never silently swallows.
    #[error("blob I/O: {0}")]
    Io(String),
}

// ─── Media reference (carried in frames) ──────────────────────────────

/// Pointer to a blob carried inside a frame body. Frames stay small;
/// the actual bytes live in the blob store and are fetched on-demand.
///
/// The recipient sees `MediaRef`, queries their local store, and if the
/// hash is not present, re-requests from peers or the originating host
/// via the transport's pull mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaRef {
    pub hash: ContentHash,
    /// Size in bytes — let recipients pre-allocate or refuse oversize
    /// before fetching.
    pub size_bytes: u64,
    /// Optional MIME hint. Pure UX/decoder hint; never load-bearing for
    /// dispatch (recipient may re-sniff after fetch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

// ─── The trait every backend implements ──────────────────────────────

/// Content-addressed blob store. Backends implement this trait;
/// callers depend only on it. Encryption / chunking / compression
/// wrappers compose by holding another `ContentAddressedStore` and
/// re-implementing the trait.
///
/// `async fn` in traits is stable in Rust 1.75+; backends use
/// `async_trait` only if they need erased trait objects.
pub trait ContentAddressedStore {
    /// Store `bytes` under their SHA-256 hash. Returns the hash on
    /// success — callers can use this to populate a `MediaRef`. If the
    /// content is already present, return Ok with the existing hash
    /// (idempotent put).
    fn put(&self, bytes: &[u8]) -> Result<ContentHash, BlobError>;

    /// Fetch the bytes for `hash`. Returns `NotFound` if absent,
    /// `HashMismatch` if stored bytes are corrupted, `Io` for backend
    /// failures.
    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, BlobError>;

    /// Probe presence without reading the bytes. Cheap on most backends
    /// (file-exists check / HEAD request). Default impl falls back to
    /// `get` and discards — backends should override when they have a
    /// cheaper path.
    fn exists(&self, hash: &ContentHash) -> Result<bool, BlobError> {
        match self.get(hash) {
            Ok(_) => Ok(true),
            Err(BlobError::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Remove a blob. Used by GC / retention layer. `NotFound` is NOT
    /// an error — delete is idempotent.
    fn delete(&self, hash: &ContentHash) -> Result<(), BlobError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What this catches: `MediaRef` round-trips through serde JSON
    /// without losing the hash, size, or optional mime. Wire-stability
    /// pin — frame senders + receivers MUST agree on this shape.
    #[test]
    fn media_ref_round_trips_through_serde() {
        let hash = ContentHash::from_bytes(b"hello world");
        let media_ref = MediaRef {
            hash: hash.clone(),
            size_bytes: 11,
            mime: Some("text/plain".to_string()),
        };
        let json = serde_json::to_string(&media_ref).expect("serialize");
        let restored: MediaRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, media_ref);
    }

    /// What this catches: optional `mime` serializes absent when None
    /// (not as `"mime": null`). Wire-efficiency pin — most blobs won't
    /// carry a mime hint, omitting saves bytes on the wire.
    #[test]
    fn media_ref_omits_none_mime_in_serde() {
        let media_ref = MediaRef {
            hash: ContentHash::from_bytes(b""),
            size_bytes: 0,
            mime: None,
        };
        let json = serde_json::to_string(&media_ref).expect("serialize");
        assert!(!json.contains("mime"), "None mime should be omitted; got: {json}");
    }

    /// In-memory fake backend used to exercise the default `exists`
    /// implementation. Real backends override `exists` with a cheap
    /// presence-check; the fallback path is correctness-preserved by
    /// this test.
    struct InMemoryStore {
        data: std::sync::Mutex<std::collections::HashMap<ContentHash, Vec<u8>>>,
    }

    impl InMemoryStore {
        fn new() -> Self {
            Self {
                data: std::sync::Mutex::new(std::collections::HashMap::new()),
            }
        }
    }

    impl ContentAddressedStore for InMemoryStore {
        fn put(&self, bytes: &[u8]) -> Result<ContentHash, BlobError> {
            let hash = ContentHash::from_bytes(bytes);
            self.data
                .lock()
                .unwrap()
                .insert(hash.clone(), bytes.to_vec());
            Ok(hash)
        }

        fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, BlobError> {
            self.data
                .lock()
                .unwrap()
                .get(hash)
                .cloned()
                .ok_or_else(|| BlobError::NotFound {
                    hash: hash.clone(),
                })
        }

        fn delete(&self, hash: &ContentHash) -> Result<(), BlobError> {
            self.data.lock().unwrap().remove(hash);
            Ok(())
        }
    }

    /// What this catches: put → get round-trip via the trait. Smoke
    /// proof that the trait is implementable + the default backend
    /// arithmetic works.
    #[test]
    fn store_put_get_round_trip() {
        let store = InMemoryStore::new();
        let hash = store.put(b"hello world").expect("put");
        let bytes = store.get(&hash).expect("get");
        assert_eq!(bytes, b"hello world");
    }

    /// What this catches: default `exists` impl reports true for stored
    /// + false for missing. Backends overriding `exists` should still
    /// match this contract.
    #[test]
    fn store_exists_default_impl_matches_get() {
        let store = InMemoryStore::new();
        let hash = store.put(b"present").expect("put");
        let absent = ContentHash::from_bytes(b"absent");
        assert!(store.exists(&hash).unwrap());
        assert!(!store.exists(&absent).unwrap());
    }

    /// What this catches: delete is idempotent — deleting an absent
    /// hash returns Ok, not Err. Saves callers a "check before delete"
    /// pattern.
    #[test]
    fn store_delete_idempotent() {
        let store = InMemoryStore::new();
        let absent = ContentHash::from_bytes(b"never_stored");
        assert!(store.delete(&absent).is_ok(), "delete of absent must be Ok");
        let hash = store.put(b"x").expect("put");
        assert!(store.delete(&hash).is_ok());
        // Double-delete also fine.
        assert!(store.delete(&hash).is_ok());
        assert!(!store.exists(&hash).unwrap());
    }

    /// What this catches: BlobError variants Display with informative
    /// text including the relevant hash (so logs surface what's
    /// missing/corrupted without a separate lookup).
    #[test]
    fn blob_error_display_includes_context() {
        let hash = ContentHash::from_bytes(b"x");
        let not_found = BlobError::NotFound {
            hash: hash.clone(),
        };
        let display = format!("{not_found}");
        assert!(display.contains(&hash.to_string()));

        let mismatch = BlobError::HashMismatch {
            expected: hash.clone(),
            actual: ContentHash::from_bytes(b"y"),
        };
        let display = format!("{mismatch}");
        assert!(display.contains(&hash.to_string()));
    }
}
