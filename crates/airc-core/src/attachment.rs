//! File-attachment manifest type.
//!
//! NOTE: this type is the consumer-side richer view of an attached file —
//! it carries display name, MIME type, optional local + remote paths,
//! and the canonical `content_hash`. The protocol-layer pointer that
//! actually flows on the wire is a separate, smaller type (`MediaRef`)
//! defined in the `airc-blobs` crate so the protocol body never carries
//! local-FS paths or unstructured remote references.
//!
//! The audit broadcast on #cambriantech called out the smell of mixing
//! protocol + consumer concerns in one struct; the resolution is the
//! split between `airc-blobs::MediaRef` (protocol) and this manifest
//! (consumer-side display). Today the wire still uses this manifest
//! shape for backward-compat with the Python+bash airc; the split lands
//! when `airc-blobs` ships its MediaRef in a follow-up PR.

use serde::{Deserialize, Serialize};

use crate::ids::{ContentHash, FileId};

/// Consumer-side metadata for one attached file.
///
/// The display fields (`name`, `media_type`, `local_path`, `remote_ref`)
/// are intended for UI rendering and consumer-side file management. The
/// `content_hash` + `size_bytes` are the protocol-canonical fields that
/// `MediaRef` (in airc-blobs) will own once the protocol/consumer split
/// lands. Until then this struct is the single representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentManifest {
    /// Stable handle for this attached file, scoped to the transcript.
    pub file_id: FileId,
    /// Display filename — what UIs render.
    pub name: String,
    /// MIME type (RFC 6838). Optional because legacy entries may lack it.
    pub media_type: Option<String>,
    /// Byte size of the underlying blob. Required: callers depend on it
    /// for buffer pre-allocation + display ("23 KB").
    pub size_bytes: u64,
    /// Content-addressed hash of the blob bytes. `"sha256:<hex>"` format
    /// in practice but typed as opaque so other algorithms can coexist.
    pub content_hash: ContentHash,
    /// Optional local filesystem path on the receiver side. Consumer-
    /// managed; airc never writes paths here itself.
    pub local_path: Option<String>,
    /// Optional remote / cloud reference (URL, S3 key, etc.). Consumer-
    /// managed and unstructured at this layer.
    pub remote_ref: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_readable_serde() {
        let file_id = FileId::from_u128(0x550e8400_e29b_41d4_a716_446655440000);
        let manifest = AttachmentManifest {
            file_id,
            name: "trace.json".to_string(),
            media_type: Some("application/json".to_string()),
            size_bytes: 42,
            content_hash: ContentHash("sha256:abc".to_string()),
            local_path: Some("/tmp/trace.json".to_string()),
            remote_ref: None,
        };

        let encoded = serde_json::to_value(&manifest).unwrap();

        // file_id serializes as the canonical hyphenated UUID string.
        assert_eq!(encoded["file_id"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(encoded["content_hash"], "sha256:abc");
        assert_eq!(encoded["size_bytes"], 42);
    }
}
