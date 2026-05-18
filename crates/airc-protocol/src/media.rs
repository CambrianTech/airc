//! Media attachment reference — pointer to a blob carried by airc-blobs.
//!
//! An `Envelope` carries `Vec<MediaRef>` rather than inlined bytes so that
//! large media never roundtrip through the substrate's message path. The
//! actual content lives in airc-blobs (sha256-addressed), and adapters
//! fetch it lazily by `content_hash` when they need the bytes.
//!
//! Why a separate type from `airc_core::AttachmentManifest`: `AttachmentManifest`
//! is the rich consumer-side view (with thumbnails, caption styling,
//! mime-derived UI hints, etc.). `MediaRef` is the *envelope* primitive —
//! just enough for a transport adapter to route the reference and for the
//! receiver to materialize it. Consumers build the richer manifest on
//! top.

use serde::{Deserialize, Serialize};

use airc_core::{ContentHash, FileId};

/// Pointer to a single blob attached to an envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRef {
    /// Stable handle for this attachment within the envelope. Used by
    /// consumers that want to reference one specific attachment among
    /// many (e.g. "the second image"). UUIDv4 per `airc_core::FileId`.
    pub file_id: FileId,

    /// Content-addressed hash of the blob bytes. Adapters fetch the
    /// blob by this hash; collision = identical content (a feature).
    pub content_hash: ContentHash,

    /// Optional MIME type hint. Receivers prefer the blob's stored
    /// metadata; this is for the case where the receiver has the
    /// envelope but not yet the blob and needs to decide whether to
    /// fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,

    /// Optional byte size hint. Same use as `mime`: lets a receiver
    /// decide "fetch now" vs. "fetch on demand" without round-tripping
    /// the blob store. Authoritative size comes from the blob itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,

    /// Optional caption / alt text. Consumer-facing. Substrate does not
    /// interpret this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_ref_roundtrips_through_serde_with_optionals_omitted() {
        let media = MediaRef {
            file_id: FileId::from_u128(0xabc),
            content_hash: ContentHash("sha256:deadbeef".to_string()),
            mime: None,
            size_bytes: None,
            caption: None,
        };
        let encoded = serde_json::to_value(&media).unwrap();
        // Optional fields with `skip_serializing_if = Option::is_none`
        // must be absent — keeps the wire form compact and stable for
        // canonical-bytes hashing.
        assert_eq!(encoded.as_object().unwrap().len(), 2);
        let decoded: MediaRef = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, media);
    }

    #[test]
    fn media_ref_carries_optional_fields_when_set() {
        let media = MediaRef {
            file_id: FileId::from_u128(0xdef),
            content_hash: ContentHash("sha256:cafebabe".to_string()),
            mime: Some("image/png".to_string()),
            size_bytes: Some(102_400),
            caption: Some("screenshot".to_string()),
        };
        let encoded = serde_json::to_value(&media).unwrap();
        assert_eq!(encoded["mime"], "image/png");
        assert_eq!(encoded["size_bytes"], 102_400);
        assert_eq!(encoded["caption"], "screenshot");
        let decoded: MediaRef = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, media);
    }
}
