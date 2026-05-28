//! Message body — the opaque payload airc carries on behalf of consumers.
//!
//! Per the airc-rust substrate design doc, a body is intentionally opaque
//! to airc: consumers serialize their own structured payloads (chat text,
//! command JSON, event JSON, signaling SDP, etc.) into either a JSON value
//! or raw binary bytes, and airc routes/persists the result without
//! interpretation.
//!
//! The previous shape (`body: Option<String>`) was text-only and forced
//! consumers to base64-encode binary data inside a string. The `Body` enum
//! enables first-class binary payloads for game-state diffs, WebRTC
//! signaling, custom binary command formats, and any consumer that needs
//! to avoid JSON overhead.
//!
//! Tagged enum (`{"kind": "json", "value": ...}` / `{"kind": "binary",
//! "value": [...]}`) so both variants round-trip through any serde
//! format. Zero airc-rust users means we don't need the legacy bare-
//! string backward-compat; the Python+bash airc keeps its own shape
//! until cutover, and airc-rust starts with the correct primitive.

use serde::{Deserialize, Serialize};

/// Opaque message body. Consumers pick the shape; airc routes/persists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Body {
    /// Structured JSON payload — the common case (chat text wrapped in a
    /// `{"text":"..."}` object, typed events, RPC commands with
    /// correlation IDs, etc.).
    Json(serde_json::Value),
    /// Raw binary bytes — for consumers that need to avoid JSON overhead
    /// (game-state diffs at high tick rate, pre-encoded media frames,
    /// custom wire formats). Always distinguishable from Json on the
    /// wire via the `"kind": "binary"` discriminator.
    Binary(Vec<u8>),
}

impl Body {
    /// Wrap a chat-style text string into a `Body::Json` with the
    /// canonical `{"text": "..."}` shape consumers conventionally use
    /// for plain chat.
    pub fn text(s: impl Into<String>) -> Self {
        Body::Json(serde_json::json!({ "text": s.into() }))
    }

    /// Extract chat-style text from a body, if the body is a JSON
    /// object with a `text` field of string type. Returns `None` for
    /// any other shape (binary, JSON without text field, etc.).
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Body::Json(serde_json::Value::Object(map)) => map.get("text").and_then(|v| v.as_str()),
            Body::Json(serde_json::Value::String(s)) => {
                // Backward-compat: legacy bare-string payloads still
                // surface as text.
                Some(s.as_str())
            }
            Body::Json(
                serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::Array(_),
            )
            | Body::Binary(_) => None,
        }
    }

    /// Encode this body as the opaque `payload` bytes of an `airc-bus`
    /// envelope. The bus/wire never parse the payload — this is airc's
    /// native-chat consumer codec, the encode half of the boundary
    /// (inverse of [`Body::from_payload`]). `Body` is always
    /// serde-serializable (`Json(Value)` | `Binary(Vec<u8>)`), so a
    /// failure here is a serializer bug, not a runtime condition.
    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Body always serializes to JSON")
    }

    /// Decode the opaque `payload` bytes of an `airc-bus` envelope back
    /// into a `Body`. The decode half of the boundary — surfaces a typed
    /// error rather than silently dropping a malformed payload.
    pub fn from_payload(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_payload_round_trips_json_and_binary() {
        let text = Body::text("hello over the bus");
        let decoded = Body::from_payload(&text.to_payload()).expect("json payload decodes");
        assert_eq!(decoded, text);
        assert_eq!(decoded.as_text(), Some("hello over the bus"));

        let bin = Body::Binary(vec![0, 1, 2, 255, 42]);
        let decoded = Body::from_payload(&bin.to_payload()).expect("binary payload decodes");
        assert_eq!(decoded, bin);
    }

    #[test]
    fn from_payload_surfaces_malformed_bytes_as_error() {
        // Not a silent drop — a malformed payload is a typed Err.
        assert!(Body::from_payload(b"\xff\x00not json").is_err());
    }

    #[test]
    fn json_body_roundtrips_through_serde() {
        let body = Body::Json(serde_json::json!({
            "text": "hello world",
            "lang": "en"
        }));
        let encoded = serde_json::to_value(&body).unwrap();
        let decoded: Body = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, body);
        // Tagged: encoding carries the discriminator + content fields.
        assert_eq!(encoded["kind"], "json");
        assert_eq!(encoded["value"]["text"], "hello world");
        assert_eq!(encoded["value"]["lang"], "en");
    }

    #[test]
    fn binary_body_roundtrips_through_serde() {
        let body = Body::Binary(vec![0x00, 0xff, 0xde, 0xad, 0xbe, 0xef]);
        let encoded = serde_json::to_value(&body).unwrap();
        let decoded: Body = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, body);
        // Tagged form distinguishes Binary from a Json(Array) of the
        // same numbers — no more "indistinguishable after roundtrip"
        // edge case the untagged shape had.
        assert_eq!(encoded["kind"], "binary");
        assert!(encoded["value"].is_array());
    }

    #[test]
    fn text_helper_constructs_canonical_chat_shape() {
        let body = Body::text("hi");
        match &body {
            Body::Json(serde_json::Value::Object(m)) => {
                assert_eq!(m["text"], "hi");
                assert_eq!(m.len(), 1, "text helper produces exactly {{text: ...}}");
            }
            Body::Json(
                serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::Number(_)
                | serde_json::Value::String(_)
                | serde_json::Value::Array(_),
            )
            | Body::Binary(_) => panic!("text() should produce Body::Json with object shape"),
        }
        assert_eq!(body.as_text(), Some("hi"));
    }

    #[test]
    fn as_text_returns_none_for_binary_body() {
        let body = Body::Binary(vec![1, 2, 3]);
        assert_eq!(body.as_text(), None);
    }

    #[test]
    fn as_text_returns_none_for_json_without_text_field() {
        let body = Body::Json(serde_json::json!({"op": "kanban/create", "args": []}));
        assert_eq!(body.as_text(), None);
    }

    #[test]
    fn json_array_of_ints_stays_json_not_binary() {
        // Edge: a JSON array of small ints would have been confused with
        // Binary under untagged serde. Tagged form makes the
        // disambiguation explicit — without "kind": "binary", it's Json.
        let body = Body::Json(serde_json::json!([1, 2, 3]));
        let encoded = serde_json::to_value(&body).unwrap();
        assert_eq!(encoded["kind"], "json");
        let decoded: Body = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn missing_kind_discriminator_is_a_parse_error() {
        // A wire envelope that drops the `kind` discriminator (e.g. a
        // bug in a non-Rust consumer that wrote a bare JSON value
        // instead of a tagged Body) should fail loud at parse time,
        // not silently default to Json. Tagged enums enforce this.
        let bare = serde_json::json!({"text": "hi"});
        let result: Result<Body, _> = serde_json::from_value(bare);
        assert!(
            result.is_err(),
            "untagged body without 'kind' must fail parse, got: {:?}",
            result
        );
    }
}
