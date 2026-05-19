//! Content hash — newtype around SHA-256.
//!
//! Identity for the blob store + reference type carried in frames. We
//! wrap the raw 32-byte digest in a newtype so the type system catches
//! "passed bytes where hash was expected" mistakes that a bare
//! `[u8; 32]` would not.
//!
//! Wire format is the hex-encoded digest (64 lowercase chars). Hex
//! beats base64 here because it is the universal `sha256sum` output;
//! grep / `find . -name '*.blob'` workflows just work.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;

/// 32-byte SHA-256 content hash. Use [`ContentHash::from_bytes`] to
/// compute, [`Display`] / [`FromStr`] for hex round-trip on the wire.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Compute the SHA-256 hash of `bytes`. The canonical constructor.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    /// Raw 32-byte digest. Use sparingly — prefer the typed `ContentHash`
    /// in APIs; the byte array is for backends that need to address
    /// disk paths or compute filenames.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse a 64-character lowercase hex string into a `ContentHash`.
    /// Returns `None` if the input isn't exactly 64 hex chars; callers
    /// surface the error rather than silently producing a wrong hash.
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0])?;
            let lo = hex_nibble(chunk[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }

    /// Hex encoding of the digest — 64 lowercase chars, matches
    /// `sha256sum` output.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for byte in self.0 {
            s.push(nibble_char(byte >> 4));
            s.push(nibble_char(byte & 0x0f));
        }
        s
    }
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn nibble_char(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + n - 10) as char,
        _ => '?', // unreachable for &0x0f-masked nibbles
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", self.to_hex())
    }
}

impl Serialize for ContentHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        ContentHash::from_hex(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "expected 64-char lowercase hex SHA-256 digest, got: {s:?}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What this catches: from_bytes hashes the input deterministically
    /// to the known SHA-256 of "hello world" (well-known test vector).
    /// If the hashing changes silently, this breaks.
    #[test]
    fn hello_world_matches_known_sha256() {
        let h = ContentHash::from_bytes(b"hello world");
        // RFC 4634 known vector
        assert_eq!(
            h.to_hex(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    /// What this catches: empty input hashes to the canonical empty-SHA-256.
    /// Boundary case that some hashing impls get wrong.
    #[test]
    fn empty_bytes_match_known_sha256() {
        let h = ContentHash::from_bytes(b"");
        assert_eq!(
            h.to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// What this catches: from_hex + to_hex round-trip preserves the
    /// digest exactly. If the encoding direction drifts, frame senders
    /// + receivers disagree.
    #[test]
    fn hex_round_trip() {
        let original = ContentHash::from_bytes(b"round-trip");
        let hex = original.to_hex();
        let restored = ContentHash::from_hex(&hex).expect("valid hex round-trip");
        assert_eq!(original, restored);
    }

    /// What this catches: from_hex rejects wrong-length input (not 64
    /// chars). Pins the boundary so a future "accept variable length"
    /// change is deliberate.
    #[test]
    fn from_hex_rejects_wrong_length() {
        assert!(ContentHash::from_hex("").is_none());
        assert!(ContentHash::from_hex("abc").is_none());
        assert!(ContentHash::from_hex(&"a".repeat(63)).is_none());
        assert!(ContentHash::from_hex(&"a".repeat(65)).is_none());
    }

    /// What this catches: from_hex rejects non-hex chars. Pins that
    /// invalid hex produces None, not garbage bytes.
    #[test]
    fn from_hex_rejects_non_hex_chars() {
        assert!(ContentHash::from_hex(&"g".repeat(64)).is_none());
        assert!(ContentHash::from_hex(&"!".repeat(64)).is_none());
    }

    /// What this catches: from_hex accepts both lowercase + uppercase
    /// (operators paste hashes from `sha256sum` or `openssl dgst -sha256`
    /// — outputs differ).
    #[test]
    fn from_hex_accepts_uppercase() {
        let lower = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let upper = lower.to_uppercase();
        let from_lower = ContentHash::from_hex(lower).expect("lower hex");
        let from_upper = ContentHash::from_hex(&upper).expect("upper hex");
        assert_eq!(from_lower, from_upper);
    }

    /// What this catches: Display uses lowercase hex (matches
    /// `sha256sum` output convention).
    #[test]
    fn display_uses_lowercase_hex() {
        let h = ContentHash::from_bytes(b"test");
        let display = format!("{h}");
        assert_eq!(
            display,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
        assert!(display
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()));
    }

    /// What this catches: serde round-trip via JSON preserves the hash.
    /// Wire-stability pin — MediaRef + frame body share this encoding.
    #[test]
    fn serde_json_round_trip() {
        let h = ContentHash::from_bytes(b"serde-test");
        let json = serde_json::to_string(&h).expect("serialize");
        // Should be a quoted hex string
        assert_eq!(json, format!("\"{}\"", h.to_hex()));
        let restored: ContentHash = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(h, restored);
    }

    /// What this catches: deserialization rejects non-hex strings with
    /// an informative error (not silently producing a wrong hash).
    #[test]
    fn serde_deserialize_rejects_invalid_hex() {
        let err = serde_json::from_str::<ContentHash>("\"not-a-hash\"")
            .expect_err("expected error on invalid hex");
        let msg = err.to_string();
        assert!(msg.contains("hex"), "error should mention hex: {msg}");
    }

    /// What this catches: PartialEq on the underlying bytes. Two
    /// `ContentHash` values are equal iff their byte content matches.
    #[test]
    fn partial_eq_compares_bytes() {
        let a = ContentHash::from_bytes(b"same");
        let b = ContentHash::from_bytes(b"same");
        let c = ContentHash::from_bytes(b"different");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
