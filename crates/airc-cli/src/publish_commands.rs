//! `airc publish` handler — thin CLI over [`Airc::publish`].
//!
//! Reads body from `--body-text` or `--body-json @file` (with `-`
//! meaning stdin), parses repeated `--header k=v` flags, calls
//! [`Airc::publish`], and writes the typed [`PublishReceipt`] as a
//! single line of JSON to stdout. Shell consumers can `jq` it
//! without any human-prose parsing.

use std::io::Read;
use std::path::Path;

use airc_core::{Body, Headers};
use airc_lib::{Airc, PublishTarget};
use airc_protocol::FrameKind;

use crate::cli::PublishFrameKind;

pub async fn run_publish(
    home: &Path,
    room: Option<String>,
    body_text: Option<String>,
    body_json: Option<String>,
    headers: Vec<String>,
    kind: PublishFrameKind,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = load_body(body_text, body_json)?;
    let parsed_headers = parse_headers(&headers)?;
    let target = match room {
        Some(name) => PublishTarget::RoomByName(name),
        None => PublishTarget::CurrentRoom,
    };

    let airc = Airc::open(home).await?;
    let receipt = airc
        .publish(target, frame_kind_from(kind), body, parsed_headers)
        .await?;

    // One-line JSON so callers can pipe into `jq` directly.
    let line = serde_json::to_string(&receipt)
        .map_err(|error| format!("serialize publish receipt: {error}"))?;
    println!("{line}");
    Ok(())
}

fn frame_kind_from(kind: PublishFrameKind) -> FrameKind {
    match kind {
        PublishFrameKind::Message => FrameKind::Message,
        PublishFrameKind::Event => FrameKind::Event,
        PublishFrameKind::Control => FrameKind::Control,
    }
}

fn load_body(
    body_text: Option<String>,
    body_json: Option<String>,
) -> Result<Body, Box<dyn std::error::Error>> {
    match (body_text, body_json) {
        (Some(text), None) => Ok(Body::text(text)),
        (None, Some(source)) => {
            let raw = read_body_source(&source)?;
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
                format!("body-json input is not valid JSON ({source:?}): {error}")
            })?;
            Ok(Body::Json(value))
        }
        (None, None) => Err("publish requires --body-text or --body-json".into()),
        (Some(_), Some(_)) => {
            // Clap's `conflicts_with` catches this normally; this
            // branch is defensive in case the args are passed
            // programmatically.
            Err("--body-text and --body-json are mutually exclusive".into())
        }
    }
}

fn read_body_source(source: &str) -> Result<String, Box<dyn std::error::Error>> {
    if source == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|error| format!("read body-json from stdin: {error}"))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(source)
            .map_err(|error| format!("read body-json file {source:?}: {error}").into())
    }
}

fn parse_headers(specs: &[String]) -> Result<Headers, Box<dyn std::error::Error>> {
    let mut headers = Headers::new();
    for spec in specs {
        let (key, value) = spec.split_once('=').ok_or_else(|| {
            format!("--header expects `key=value`, got {spec:?} (no `=` separator)")
        })?;
        if key.is_empty() {
            return Err(format!("--header has empty key in {spec:?}").into());
        }
        headers.insert(key.into(), value.into());
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_headers_accepts_repeated_kv_pairs_in_order() {
        let parsed = parse_headers(&[
            "airc.bridge.source=slack".to_string(),
            "x.trace=abc-123".to_string(),
        ])
        .expect("ok");
        assert_eq!(
            parsed.get("airc.bridge.source").map(String::as_str),
            Some("slack")
        );
        assert_eq!(parsed.get("x.trace").map(String::as_str), Some("abc-123"));
    }

    #[test]
    fn parse_headers_preserves_empty_value() {
        let parsed = parse_headers(&["x.flag=".to_string()]).expect("ok");
        assert_eq!(parsed.get("x.flag").map(String::as_str), Some(""));
    }

    #[test]
    fn parse_headers_rejects_missing_separator() {
        let err = parse_headers(&["nope-no-equals".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("no `=`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_headers_rejects_empty_key() {
        let err = parse_headers(&["=value".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("empty key"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_body_text_wraps_string_in_canonical_chat_json_shape() {
        // `Body::text` is sugar for `Body::Json({"text": "..."})` —
        // the canonical chat shape. Confirm the CLI sugar
        // round-trips through it correctly.
        match load_body(Some("hello".into()), None).expect("ok") {
            Body::Json(value) => assert_eq!(value["text"], "hello"),
            other => panic!("expected json-wrapped text body, got {other:?}"),
        }
    }

    #[test]
    fn load_body_requires_one_source() {
        let err = load_body(None, None).unwrap_err();
        assert!(
            err.to_string()
                .contains("requires --body-text or --body-json"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_body_rejects_invalid_json() {
        // Write a temp file with bad JSON.
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), b"{ not json }").expect("write");
        let err = load_body(None, Some(tmp.path().to_string_lossy().into_owned())).unwrap_err();
        assert!(
            err.to_string().contains("not valid JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_body_json_file_parses() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), br#"{"kind":"chat","text":"hi"}"#).expect("write");
        match load_body(None, Some(tmp.path().to_string_lossy().into_owned())).expect("ok") {
            Body::Json(value) => {
                assert_eq!(value["kind"], "chat");
                assert_eq!(value["text"], "hi");
            }
            other => panic!("expected json body, got {other:?}"),
        }
    }
}
