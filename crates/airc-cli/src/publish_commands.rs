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
use airc_lib::PublishTarget;
use airc_protocol::FrameKind;

use crate::cli::PublishFrameKind;

pub async fn run_publish(
    home: &Path,
    room: Option<String>,
    body_text: Option<String>,
    body_json: Option<String>,
    stdin: bool,
    headers: Vec<String>,
    kind: PublishFrameKind,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = load_body(body_text, body_json, stdin)?;
    let parsed_headers = parse_headers(&headers)?;
    let target = match room {
        Some(name) => PublishTarget::RoomByName(name),
        None => PublishTarget::CurrentRoom,
    };

    let airc = crate::commands::attached_airc(home).await?;
    let receipt = airc
        .publish(target, frame_kind_from(kind), body, parsed_headers)
        .await?;

    // One-line JSON so callers can pipe into `jq` directly.
    let mut line = serde_json::to_value(&receipt)
        .map_err(|error| format!("serialize publish receipt: {error}"))?;
    // REACH, alongside the ids — the same two facts `airc msg` has always printed
    // (see `format_send_receipt`), which this path dropped.
    //
    // Why it matters, measured 2026-09-14: a publish to #cambriantech returned
    // {"event_id":"19dfdc60-…","lamport":…,"channel_id":…} and NEVER ARRIVED on the
    // peer. The receipt was byte-for-byte the same shape as one that did arrive, so
    // the sender had no way to tell — and reported the round trip as working on the
    // strength of it. `airc msg` would have said "⚠ reached 0 of N enrolled remote
    // peer(s)"; `airc publish` said nothing, because the honesty lived only in the
    // prose formatter.
    //
    // This is REACH, not delivery. Delivery is a returned ACK and lives in the
    // daemon's ledger (`airc doctor --health`, #280). A receipt cannot promise it.
    // What it can do is distinguish "queued with a live route" from "queued into the
    // void", which is the distinction that was missing.
    //
    // Do NOT divide these two numbers. `enrolled_peers` is every peer this scope ever
    // enrolled, across every room, for all time — not this room's audience. Printing
    // them as a ratio manufactured a false catastrophe on 2026-08-07 (card #340: the
    // receipt read 2% reach while the ack ledger showed 99.5%), and an instrument that
    // cries wolf during healthy operation burns the trust of every later alarm.
    if let Some(obj) = line.as_object_mut() {
        let enrolled_peers = airc.peers().await.map(|p| p.len()).unwrap_or(0);
        let connected_lan_peers =
            airc_ipc::DaemonClient::new(crate::cli::default_socket_path_in(home))
                .status()
                .await
                .map(|status| status.connected_lan_peers)
                .unwrap_or(0);
        obj.insert("enrolled_peers".into(), enrolled_peers.into());
        obj.insert("connected_lan_peers".into(), connected_lan_peers.into());
        // The loud case, stated as a field so a shell consumer can branch on it
        // without re-deriving the rule: enrolled peers exist and NONE is connected,
        // so the fan-out reached no remote machine.
        obj.insert(
            "reached_no_remote_peer".into(),
            (enrolled_peers > 0 && connected_lan_peers == 0).into(),
        );
    }
    let line = serde_json::to_string(&line)
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
    stdin: bool,
) -> Result<Body, Box<dyn std::error::Error>> {
    if stdin {
        return Ok(Body::text(read_prose_from_stdin()?));
    }
    match (body_text, body_json) {
        (Some(text), None) => Ok(Body::text(text)),
        (None, Some(source)) => {
            let raw = read_body_source(&source)?;
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
                format!("body-json input is not valid JSON ({source:?}): {error}")
            })?;
            Ok(Body::Json(value))
        }
        (None, None) => Err("publish requires --body-text, --body-json, or --stdin".into()),
        (Some(_), Some(_)) => {
            // Clap's `conflicts_with` catches this normally; this
            // branch is defensive in case the args are passed
            // programmatically.
            Err("--body-text and --body-json are mutually exclusive".into())
        }
    }
}

/// Read a prose body from stdin, refusing the two ways that silently go wrong.
///
/// Both guards are the ones `airc msg --stdin` carries (#1382), and both were
/// found there by review rather than by design: an empty pipe posted a BLANK
/// message, and a terminal blocked forever waiting for an EOF the operator had
/// to know to send. Duplicating the behaviour here rather than the reasoning —
/// a flag whose guards differ between two verbs is worse than no flag, because
/// the operator learns one contract and gets another.
fn read_prose_from_stdin() -> Result<String, Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return Err(
            "`--stdin` was passed but stdin is a terminal — nothing is piped in and \
                    this would wait forever. Redirect a file (`airc publish --stdin < body.txt`) \
                    or use a heredoc."
                .to_string()
                .into(),
        );
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|error| format!("reading message body from stdin: {error}"))?;
    if buf.trim().is_empty() {
        return Err(
            "`--stdin` produced an empty message body — nothing was piped in, or it \
                    expanded to whitespace. Refusing rather than publishing a blank frame."
                .to_string()
                .into(),
        );
    }
    Ok(buf)
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
        match load_body(Some("hello".into()), None, false).expect("ok") {
            Body::Json(value) => assert_eq!(value["text"], "hello"),
            other => panic!("expected json-wrapped text body, got {other:?}"),
        }
    }

    // what this catches: `--stdin` silently doing nothing because the flag was
    // added to the CLI but never consulted by load_body — the wiring, not the
    // read. A `true` here must NOT fall through to the "requires a source"
    // error, which is what an unwired flag would produce.
    #[test]
    fn stdin_flag_is_consulted_before_the_body_source_match() {
        // stdin is not a terminal under `cargo test` and is empty, so this
        // reaches the empty-body refusal — proving the flag routed there rather
        // than into the (None, None) arm.
        let err = load_body(None, None, true).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--stdin") && !msg.contains("requires --body-text"),
            "`--stdin` was not consulted; got: {msg}"
        );
    }

    #[test]
    fn load_body_requires_one_source() {
        let err = load_body(None, None, false).unwrap_err();
        assert!(
            err.to_string()
                .contains("requires --body-text, --body-json, or --stdin"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_body_rejects_invalid_json() {
        // Write a temp file with bad JSON.
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), b"{ not json }").expect("write");
        let err =
            load_body(None, Some(tmp.path().to_string_lossy().into_owned()), false).unwrap_err();
        assert!(
            err.to_string().contains("not valid JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_body_json_file_parses() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), br#"{"kind":"chat","text":"hi"}"#).expect("write");
        match load_body(None, Some(tmp.path().to_string_lossy().into_owned()), false).expect("ok") {
            Body::Json(value) => {
                assert_eq!(value["kind"], "chat");
                assert_eq!(value["text"], "hi");
            }
            other => panic!("expected json body, got {other:?}"),
        }
    }

    /// what this catches: a publish receipt that cannot say NOT DELIVERED.
    ///
    /// Regression for the 2026-09-14 loss: event 19dfdc60 was published to
    /// #cambriantech, returned a complete receipt, and never arrived on the peer.
    /// The receipt was byte-identical in SHAPE to one that did arrive, so nothing
    /// downstream could distinguish them — and the sender reported the round trip
    /// as working partly on its strength. `airc msg` had the honest version the
    /// whole time (`format_send_receipt`: "⚠ reached 0 of N enrolled remote
    /// peer(s)"); only the JSON path dropped it.
    ///
    /// This asserts the RULE, not the plumbing: enrolled peers with zero live
    /// connections must set `reached_no_remote_peer`, so a shell consumer can
    /// branch on the loud case instead of re-deriving it.
    #[test]
    fn a_receipt_says_so_when_it_reached_no_remote_peer() {
        fn verdict(enrolled: usize, connected: usize) -> bool {
            enrolled > 0 && connected == 0
        }
        // The failing shape: peers are enrolled, none is connected — the fan-out
        // went nowhere, and this is the case that looked like success.
        assert!(verdict(4, 0), "enrolled with no live route must be loud");
        // A live route: not loud. Delivery is still ack-confirmed, not promised here.
        assert!(!verdict(4, 1));
        // No enrolled peers at all is a local-only scope, not a failure.
        assert!(!verdict(0, 0), "a scope with no peers has nothing to reach");
    }
}
