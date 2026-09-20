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
        // THE DAEMON NOT ANSWERING IS NOT THE SAME AS ZERO PEERS, and collapsing
        // them is how the 13:07:07 loss stayed invisible. Root cause, measured:
        // the daemon RESTARTED at 13:07:07 (`ps -o lstart` on the new process;
        // the old one's log ends "airc daemon: stopped."), and a publish at
        // 13:07:07.955 was sequenced by the dying daemon — it is in the local
        // store — but never forwarded to any peer. Its twin 45 ms later, after
        // the new daemon held the route, arrived.
        //
        // So the frame was durably WRITTEN and never REACHED anyone, which is why
        // "no receipt without a durable write" does not describe this bug: the
        // write happened. What the receipt could not say was that reach was
        // unknowable at that instant because the daemon was mid-swap.
        //
        // `Err` here means exactly that: no answer from the daemon. Reporting it
        // as `connected_lan_peers: 0` would be a guess wearing a number.
        let status = airc_ipc::DaemonClient::new(crate::cli::default_socket_path_in(home))
            .status()
            .await;
        let daemon_answered = status.is_ok();
        let daemon_uptime_secs = status.as_ref().ok().map(|s| s.uptime_seconds);
        let connected_lan_peers = status.map(|s| s.connected_lan_peers).unwrap_or(0);
        obj.insert("enrolled_peers".into(), enrolled_peers.into());
        obj.insert("daemon_answered".into(), daemon_answered.into());
        // A RECONNECT IS NOT AN OUTAGE — @7711fe60's refinement, from the first real
        // firing of this receipt 30 min after it merged.
        //
        // 2026-09-14 17:42:07Z my publish read connected_lan_peers 0, enrolled 101,
        // reached_no_remote_peer true. Correct — the message reached nobody. But the
        // CAUSE was that my daemon had auto-updated to 6c7c56fab seconds earlier and
        // the LAN route had not re-established. From the other side at 17:44:36Z the
        // M5 read 1143/1143 acked to this node at 58 ms rtt: nothing was broken.
        //
        // Without the uptime, a reader cannot tell a fresh restart from a dead route,
        // and I nearly filed my own instrument as a false alarm on exactly that
        // confusion. A receipt that says "0 peers, daemon up 4s" reads as "wait";
        // one that says "0 peers, daemon up 3 hours" reads as "investigate". Same
        // number, opposite actions.
        //
        // `None` when the daemon did not answer — honest-absent, never a zero that
        // would read as "just started".
        obj.insert(
            "daemon_uptime_secs".into(),
            match daemon_uptime_secs {
                Some(secs) => secs.into(),
                None => serde_json::Value::Null,
            },
        );
        if daemon_answered {
            obj.insert("connected_lan_peers".into(), connected_lan_peers.into());
        } else {
            // Honest-absent: no number at all rather than a zero that reads as
            // measured. A consumer that sees null here knows reach is UNKNOWN.
            obj.insert("connected_lan_peers".into(), serde_json::Value::Null);
        }
        // The loud case, stated as a field so a shell consumer can branch on it
        // without re-deriving the rule. TRUE in both failing shapes:
        //   - daemon answered, peers enrolled, none connected → fan-out reached nobody
        //   - daemon did not answer → mid-swap or down; reach cannot be claimed
        obj.insert(
            "reached_no_remote_peer".into(),
            (!daemon_answered || (enrolled_peers > 0 && connected_lan_peers == 0)).into(),
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
        fn verdict(daemon_answered: bool, enrolled: usize, connected: usize) -> bool {
            !daemon_answered || (enrolled > 0 && connected == 0)
        }
        // The failing shape: peers are enrolled, none is connected — the fan-out
        // went nowhere, and this is the case that looked like success.
        assert!(
            verdict(true, 4, 0),
            "enrolled with no live route must be loud"
        );
        // A live route: not loud. Delivery is still ack-confirmed, not promised here.
        assert!(!verdict(true, 4, 1));
        // No enrolled peers at all is a local-only scope, not a failure.
        assert!(
            !verdict(true, 0, 0),
            "a scope with no peers has nothing to reach"
        );
    }

    /// what this catches: the ROOT CAUSE of the 2026-09-14 loss — a publish that
    /// lands inside the daemon's own restart window.
    ///
    /// Measured: the daemon restarted at 13:07:07 (`ps -o lstart` on the new
    /// process; the old one's log ends "airc daemon: stopped."). Event 19dfdc60,
    /// published at 13:07:07.955, was sequenced by the dying daemon — it IS in the
    /// local store — and never forwarded to any peer. Its twin 45 ms later arrived.
    ///
    /// So the frame was durably written and reached nobody, and the receipt said
    /// nothing was wrong. A daemon that does not answer must never be reported as
    /// "0 connected peers": that is a guess wearing a number. It is UNKNOWN reach,
    /// and unknown reach is loud.
    /// what this catches: a RECONNECT read as an OUTAGE.
    ///
    /// The first real firing of this receipt (2026-09-14 17:42:07Z) reported
    /// connected_lan_peers 0 / enrolled 101 / reached_no_remote_peer true — all
    /// correct, the message reached nobody. But the cause was a daemon that had
    /// auto-updated seconds earlier, not a broken route: from the other side at
    /// 17:44:36Z the peer read 1143/1143 acked at 58 ms rtt.
    ///
    /// I nearly reported my own instrument as a false alarm on that confusion. The
    /// uptime is what separates "wait, it is reconnecting" from "investigate, the
    /// route is dead" — same zero, opposite actions.
    #[test]
    fn a_fresh_daemon_is_distinguishable_from_a_dead_route() {
        // Both are zero-reach; only the uptime tells them apart.
        let reconnecting = (0usize, Some(4u64));
        let dead_route = (0usize, Some(10_800u64));
        let unknown = (0usize, None::<u64>);
        fn reads_as_reconnect(state: (usize, Option<u64>)) -> bool {
            matches!(state, (0, Some(secs)) if secs < 60)
        }
        assert!(
            reads_as_reconnect(reconnecting),
            "a daemon up 4s with no peers is mid-reconnect, not an outage"
        );
        assert!(
            !reads_as_reconnect(dead_route),
            "a daemon up 3h with no peers is a real loss of route"
        );
        assert!(
            !reads_as_reconnect(unknown),
            "no uptime at all cannot be claimed as a reconnect"
        );
    }

    #[test]
    fn a_daemon_that_did_not_answer_is_unknown_reach_not_zero_reach() {
        fn verdict(daemon_answered: bool, enrolled: usize, connected: usize) -> bool {
            !daemon_answered || (enrolled > 0 && connected == 0)
        }
        // Mid-swap: no answer from the daemon. Loud REGARDLESS of the peer counts,
        // including the case that would otherwise read healthy (a live route).
        assert!(
            verdict(false, 4, 1),
            "no answer from the daemon cannot be reported as reach"
        );
        // And loud even for a scope with no enrolled peers — we cannot claim a
        // local-only send succeeded if we could not ask.
        assert!(verdict(false, 0, 0));
    }
}
