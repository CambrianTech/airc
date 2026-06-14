//! `airc codex-hook ...` handlers.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use airc_core::{Body, TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, EventFilter, LiveLag};
use airc_protocol::HEADER_AIRC_CLIENT;
use futures::StreamExt;
use serde::Serialize;

use crate::client_id::current_client_id;
use crate::work_suggestions::{is_work_queue_event, render_claimable_work};

const CONSUMER_PREFIX: &str = "codex-hook";

pub async fn run_user_prompt_submit(
    home: &Path,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    drain_stdin()?;

    let airc = crate::commands::attached_airc(home).await?;
    let filter = hook_filter();
    let runtime_client = current_client_id()?;
    let consumer_id = consumer_id(runtime_client.as_deref());
    let events = unread_events(&airc, &consumer_id, filter, count).await?;

    if let Some(newest) = events.last() {
        airc.save_runtime_cursor_for_event(&consumer_id, newest)
            .await?;
    }

    let work_context = work_context(&airc, &events).await?;
    let visible: Vec<_> = events
        .into_iter()
        .filter(|event| include_self || !is_self_event(event, &airc, runtime_client.as_deref()))
        .collect();
    let Some(context) = render_context(&airc, &visible, max_items, raw, work_context).await? else {
        return Ok(());
    };
    print_hook_payload(&context)?;
    Ok(())
}

pub async fn run_poll(
    home: &Path,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
    wait_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let runtime_client = current_client_id()?;
    let consumer_id = consumer_id(runtime_client.as_deref());
    let filter = hook_filter();

    let mut events = unread_events(&airc, &consumer_id, filter.clone(), count).await?;
    if events.is_empty() && wait_ms > 0 {
        events = wait_for_one_event(&airc, filter.clone(), wait_ms).await?;
        if events.is_empty() {
            events = unread_events(&airc, &consumer_id, filter, count).await?;
        }
    }

    if let Some(newest) = events.last() {
        airc.save_runtime_cursor_for_event(&consumer_id, newest)
            .await?;
    }

    let work_context = work_context(&airc, &events).await?;
    let visible: Vec<_> = events
        .into_iter()
        .filter(|event| include_self || !is_self_event(event, &airc, runtime_client.as_deref()))
        .collect();
    let Some(context) = render_context(&airc, &visible, max_items, raw, work_context).await? else {
        return Ok(());
    };
    println!("{context}");
    Ok(())
}

async fn work_context(
    airc: &Airc,
    events: &[TranscriptEvent],
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if events.iter().any(is_work_queue_event) {
        render_claimable_work(airc).await
    } else {
        Ok(None)
    }
}

async fn render_context(
    _airc: &Airc,
    events: &[TranscriptEvent],
    max_items: usize,
    raw: bool,
    work_context: Option<String>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let chat_context = if raw {
        let text = render_raw(events);
        (!text.is_empty()).then_some(text)
    } else {
        let text = render_digest(events, max_items);
        (!text.is_empty()).then_some(text)
    };
    let mut sections = Vec::new();
    if let Some(context) = chat_context {
        sections.push(context);
    }
    if let Some(context) = work_context {
        sections.push(context);
    }
    Ok((!sections.is_empty()).then(|| sections.join("\n\n")))
}

fn consumer_id(runtime_client: Option<&str>) -> String {
    match runtime_client {
        Some(client) => format!("{CONSUMER_PREFIX}:{client}"),
        None => format!("{CONSUMER_PREFIX}:default"),
    }
}

fn hook_filter() -> EventFilter {
    EventFilter {
        kinds: BTreeSet::from([TranscriptKind::Message, TranscriptKind::System]),
        ..EventFilter::default()
    }
}

async fn unread_events(
    airc: &Airc,
    consumer_id: &str,
    filter: EventFilter,
    count: usize,
) -> Result<Vec<TranscriptEvent>, Box<dyn std::error::Error>> {
    let previous = airc.load_runtime_cursor(consumer_id).await?;
    let events = match previous {
        Some(cursor) => {
            airc.resume_from_subscribed_filtered(&cursor, filter, count)
                .await?
        }
        None => airc.page_recent_subscribed_filtered(filter, count).await?,
    };
    Ok(events)
}

async fn wait_for_one_event(
    airc: &Airc,
    filter: EventFilter,
    wait_ms: u64,
) -> Result<Vec<TranscriptEvent>, Box<dyn std::error::Error>> {
    let mut stream = airc.subscribe_subscribed_filtered(filter).await?;
    match tokio::time::timeout(Duration::from_millis(wait_ms), stream.next()).await {
        Ok(Some(Ok(event))) => Ok(vec![event.as_ref().clone()]),
        Ok(Some(Err(LiveLag { .. }))) | Ok(None) | Err(_) => Ok(Vec::new()),
    }
}

fn is_self_event(event: &TranscriptEvent, airc: &Airc, runtime_client: Option<&str>) -> bool {
    // Header-stamped self-detection is the only reliable signal on a
    // shared HOME. Two scopes (Claude tab + Codex tab) using the
    // same `~/.airc/` share the singleton local identity row →
    // share peer_id AND client_id. The peer_id/client_id equality
    // check would incorrectly classify EVERY frame from the other
    // runtime as "self" and filter it out.
    //
    // Rule: if the event has an `airc.client` header, that IS the
    // self attestation. Equal to OUR runtime client tag → self.
    // Different (or our tag absent) → NOT self. Peer_id never
    // overrides a stamped header on a shared HOME.
    if let Some(event_client) = event.headers.get(HEADER_AIRC_CLIENT) {
        return runtime_client.is_some_and(|rc| event_client == rc);
    }
    // No header: unstamped historical frame. Drop the peer_id check
    // entirely (Codex's review on PR #869): peer_id is too coarse
    // for shared-HOME multi-agent operation — every cross-runtime
    // frame would be suppressed. client_id alone is still subject
    // to identity-collision on shared HOME, but it's a tighter
    // signal than peer_id. Current Rust-emitted frames always stamp
    // the airc.client header in send_frame, so this branch only
    // fires for historical frames where false-self
    // suppression is the lesser harm.
    event.client_id == airc.client_id()
}

/// Read Codex's hook JSON from stdin, with a hard deadline so a
/// hung pipe never deadlocks the process.
///
/// **Why the deadline exists** (airc#1097). On the Windows CI
/// runner, the codex_hook_commands tests hung for 5+ hours because
/// `std::io::stdin().read_to_string` waited forever for an EOF that
/// never arrived — most likely a Windows handle-inheritance leak
/// from `Stdio::piped()` parent → grandchild daemon, where the
/// daemon's inherited duplicate of the pipe-read handle kept the
/// pipe alive after the parent closed its write end. Mac/Linux
/// don't manifest the issue (~1s tests there).
///
/// The robust fix isn't a platform-specific handle-inheritance
/// patch (untestable without a Windows machine, fragile across
/// future cargo / std / Windows changes). It's to **stop relying
/// on stdin-EOF as a liveness signal**: read with a deadline,
/// proceed with empty payload if EOF doesn't arrive in time.
/// Codex sends the JSON in microseconds in the happy path; 5s
/// is far past any legitimate latency.
///
/// On Mac/Linux this changes nothing observable (EOF arrives
/// well within the deadline). On Windows the deadlock becomes a
/// fast-fail-and-proceed — the hook produces no AdditionalContext
/// for that call, which is the same outcome as receiving empty
/// stdin, and the next hook invocation behaves correctly.
///
/// **Intentional trade-offs the deadline introduces** (BIGMAMA
/// review BLOCK#2 + #3 on PR #1197):
///
/// 1. **Slow-but-legitimate producer truncates to empty payload.**
///    The deadline is on EOF, not on byte progress. A producer that
///    streams valid JSON byte-by-byte over >5s gets its input
///    discarded — `read_to_string` never returns, so the partial
///    buffer in the reader thread is dropped and the hook proceeds
///    with empty payload. The current caller (Codex) flushes JSON in
///    microseconds; if a future caller streams slowly, the deadline
///    needs to reset on progress or read to a JSON-object delimiter
///    rather than EOF. Today we accept the truncation because the
///    alternative is "Windows CI hangs for hours."
///
/// 2. **Timeout silently skips JSON validation.** The EOF path
///    rejects non-object input with a hard error
///    (`!value.is_object()` → `Err(...)` → non-zero exit). The
///    timeout path returns `Ok(())` unconditionally — so "malformed
///    input that also arrives slowly" degrades from a hard error to
///    a silent success. The `eprintln!` distinguishes timeout from
///    EOF in logs, but the validation-skip is intentional: a hook
///    that exits non-zero on slow-malformed input would block every
///    Codex prompt-submit on the same Windows hang it was meant to
///    end. Operators see WHY via stderr; the input contract is
///    "validates iff EOF arrives in time."
///
/// Both trade-offs are pinned by
/// `drain_stdin_timeout_proceeds_when_eof_never_arrives` in
/// `tests/codex_hook_commands.rs` — see the regression test for the
/// exact failure shape if a future refactor reverts the deadline.
fn drain_stdin() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    const STDIN_READ_DEADLINE: Duration = Duration::from_secs(5);

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut raw = String::new();
        let result = std::io::stdin().read_to_string(&mut raw).map(|_| raw);
        let _ = tx.send(result);
    });

    let raw = match rx.recv_timeout(STDIN_READ_DEADLINE) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e.into()),
        Err(_timeout) => {
            // Best-effort diagnostic; the orphan reader thread keeps
            // its handle and will exit when the OS reclaims it after
            // process exit.
            eprintln!(
                "airc codex-hook: stdin EOF not received within {STDIN_READ_DEADLINE:?}, \
                 proceeding with empty payload (see airc#1097)"
            );
            return Ok(());
        }
    };

    if raw.trim().is_empty() {
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err("Codex hook stdin must be a JSON object".into());
    }
    Ok(())
}

fn print_hook_payload(context: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HookPayload<'a> {
        hook_specific_output: HookSpecificOutput<'a>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HookSpecificOutput<'a> {
        hook_event_name: &'a str,
        additional_context: &'a str,
    }

    let payload = HookPayload {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "UserPromptSubmit",
            additional_context: context,
        },
    };
    println!("{}", serde_json::to_string(&payload)?);
    Ok(())
}

fn render_raw(events: &[TranscriptEvent]) -> String {
    events
        .iter()
        .map(|event| {
            format!(
                "[{}] {}: {}",
                event.occurred_at_ms,
                event.peer_id,
                summarize_body(event)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_digest(events: &[TranscriptEvent], max_items: usize) -> String {
    let messages = dedupe(events);
    if messages.is_empty() {
        return String::new();
    }

    let max_items = max_items.max(1);
    let hidden = messages.len().saturating_sub(max_items);
    let shown = &messages[messages.len().saturating_sub(max_items)..];
    let mut peers = Vec::new();
    for message in &messages {
        if !peers.contains(&message.peer) {
            peers.push(message.peer.clone());
        }
    }

    let mut lines = Vec::new();
    let mut first = format!("AIRC: {} unread", messages.len());
    if !peers.is_empty() {
        let shown_peers = peers.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        first.push_str(" from ");
        first.push_str(&shown_peers);
        if peers.len() > 3 {
            first.push_str(&format!(" +{}", peers.len() - 3));
        }
    }
    lines.push(first);
    if hidden > 0 {
        lines.push(format!(
            "latest {} shown; {hidden} older omitted",
            shown.len()
        ));
    }
    for message in shown {
        lines.push(format!(
            "- {}: {}",
            message.peer,
            summarize_text(&message.body, 120)
        ));
    }
    if hidden > 0 {
        lines.push("more: airc codex-hook user-prompt-submit --raw".to_string());
    }
    lines.join("\n")
}

#[derive(Clone)]
struct DigestMessage {
    peer: String,
    body: String,
}

fn dedupe(events: &[TranscriptEvent]) -> Vec<DigestMessage> {
    let mut seen = BTreeSet::new();
    let mut messages = Vec::new();
    for event in events {
        if is_work_queue_event(event) || is_runtime_alive_event(event) {
            continue;
        }
        let peer = event.peer_id.to_string();
        let body = summarize_body(event);
        if seen.insert((peer.clone(), body.clone())) {
            messages.push(DigestMessage { peer, body });
        }
    }
    messages
}

fn is_runtime_alive_event(event: &TranscriptEvent) -> bool {
    let Some(body) = &event.body else {
        return false;
    };
    match body {
        Body::Binary(_) => false,
        Body::Json(value) => {
            body_kind_is_alive(value)
                || body.as_text().is_some_and(|text| {
                    serde_json::from_str::<serde_json::Value>(text)
                        .is_ok_and(|value| body_kind_is_alive(&value))
                })
        }
    }
}

fn body_kind_is_alive(value: &serde_json::Value) -> bool {
    value.get("kind").and_then(|kind| kind.as_str()) == Some("alive")
}

fn summarize_body(event: &TranscriptEvent) -> String {
    match &event.body {
        Some(body) => format_body(body),
        None => "<empty body>".to_string(),
    }
}

fn format_body(body: &Body) -> String {
    if let Some(text) = body.as_text() {
        return text.to_string();
    }
    match body {
        Body::Json(value) => value.to_string(),
        Body::Binary(bytes) => format!("<binary {} bytes>", bytes.len()),
    }
}

fn summarize_text(value: &str, max_len: usize) -> String {
    let one_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= max_len {
        return one_line;
    }
    format!("{}...", one_line[..max_len.saturating_sub(3)].trim_end())
}

#[cfg(test)]
mod tests {
    use airc_core::{
        Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
        TranscriptKind,
    };
    use serde_json::json;

    use super::{consumer_id, render_digest, render_raw};

    #[test]
    fn consumer_id_uses_runtime_client_when_present() {
        assert_eq!(
            consumer_id(Some("codex:thread-1")),
            "codex-hook:codex:thread-1"
        );
        assert_eq!(consumer_id(None), "codex-hook:default");
    }

    #[test]
    fn digest_suppresses_runtime_alive_events() {
        let events = vec![
            event(Body::Json(json!({
                "kind": "alive",
                "client_id": "claude:session-1",
            }))),
            event(Body::text("real update")),
        ];

        let digest = render_digest(&events, 8);

        assert!(digest.contains("AIRC: 1 unread"));
        assert!(digest.contains("real update"));
        assert!(!digest.contains("\"kind\":\"alive\""));
    }

    #[test]
    fn raw_output_keeps_runtime_alive_events_for_debugging() {
        let events = vec![event(Body::Json(json!({ "kind": "alive" })))];

        let raw = render_raw(&events);

        assert!(raw.contains("\"kind\":\"alive\""));
    }

    #[test]
    fn digest_suppresses_text_encoded_runtime_alive_events() {
        let events = vec![event(Body::text(r#"{"kind":"alive"}"#))];

        let digest = render_digest(&events, 8);

        assert!(digest.is_empty());
    }

    fn event(body: Body) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::System,
            occurred_at_ms: 1,
            lamport: 1,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(body),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }
}
