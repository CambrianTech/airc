//! `airc codex-hook ...` handlers.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use airc_core::{Body, TranscriptCursor, TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, EventFilter, EventScan, LiveLag};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::client_id::{
    agent_process_client_id, current_client_id, explicit_client_id, RuntimeSelfFilter,
};
use crate::work_suggestions::{is_work_queue_event, render_claimable_work};

const CONSUMER_PREFIX: &str = "codex-hook";

pub async fn run_user_prompt_submit(
    home: &Path,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    run_hook(
        home,
        count,
        max_items,
        raw,
        include_self,
        "UserPromptSubmit",
    )
    .await
}

pub async fn run_post_tool_use(
    home: &Path,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    run_hook(home, count, max_items, raw, include_self, "PostToolUse").await
}

async fn run_hook(
    home: &Path,
    count: usize,
    max_items: usize,
    raw: bool,
    include_self: bool,
    event_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let hook_session = drain_stdin()?;

    let airc = crate::commands::attached_airc(home).await?;
    let filter = hook_filter();
    // An identity the runtime states outright wins; then the hook's own session, which
    // names this exact Codex task; only then the ancestor-process guess. The guess ranked
    // above the session made every hook call under one agent ancestor share a cursor (on
    // a dev box run from Claude Code, two Codex sessions read as one).
    let runtime_client = match explicit_client_id().or(hook_session) {
        Some(client) => Some(client),
        None => agent_process_client_id()?,
    };
    let consumer_id = consumer_id(runtime_client.as_deref());
    let (batch, scanned_count) = unread_events(&airc, &consumer_id, filter, count).await?;
    let self_filter = RuntimeSelfFilter::new(airc.client_id(), runtime_client.as_deref());
    let end = delivery_end(&batch.events, max_items, raw, |event| {
        include_self || !self_filter.is_self_event(event)
    });
    let events = &batch.events[..end];

    let resume = match crate::agent_resume::prepare(
        &airc,
        &consumer_id,
        event_name == "PostToolUse",
    )
    .await
    {
        Ok(delivery) => delivery,
        Err(error) => {
            eprintln!("airc: resume context unavailable: {error}");
            None
        }
    };
    let work_context = match resume.as_ref() {
        Some(delivery) => Some(delivery.text.clone()),
        None => work_context(&airc, events).await?,
    };
    let visible: Vec<_> = events
        .iter()
        .filter(|event| include_self || !self_filter.is_self_event(event))
        .collect();
    if let Some(context) = continuation(
        render_context(&visible, max_items, raw, work_context),
        end < batch.events.len(),
        scanned_count >= count.max(1),
        runtime_client.as_deref(),
    ) {
        print_hook_payload(&context, event_name)?;
        std::io::stdout().flush()?;
        if let Some(delivery) = resume {
            if let Err(error) = crate::agent_resume::acknowledge(&airc, delivery).await {
                eprintln!("airc: resume acknowledgment failed; context remains due: {error}");
            }
        }
    }
    if let Some(cursor) = delivered_cursor(&batch, end) {
        airc.save_runtime_scan_cursor(&consumer_id, &cursor).await?;
    }

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

    let (batch, scanned_count) = poll_events_with_wake(
        &airc,
        &consumer_id,
        filter.clone(),
        count,
        wait_ms,
        wait_for_one_event(&airc, filter, wait_ms),
    )
    .await?;

    let self_filter = RuntimeSelfFilter::new(airc.client_id(), runtime_client.as_deref());
    let end = delivery_end(&batch.events, max_items, raw, |event| {
        include_self || !self_filter.is_self_event(event)
    });
    let events = &batch.events[..end];
    let work_context = work_context(&airc, events).await?;
    let visible: Vec<_> = events
        .iter()
        .filter(|event| include_self || !self_filter.is_self_event(event))
        .collect();
    if let Some(context) = continuation(
        render_context(&visible, max_items, raw, work_context),
        end < batch.events.len(),
        scanned_count >= count.max(1),
        runtime_client.as_deref(),
    ) {
        let mut output = std::io::stdout().lock();
        writeln!(output, "{context}")?;
        output.flush()?;
    }
    if let Some(cursor) = delivered_cursor(&batch, end) {
        airc.save_runtime_scan_cursor(&consumer_id, &cursor).await?;
    }

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

fn render_context(
    events: &[&TranscriptEvent],
    max_items: usize,
    raw: bool,
    work_context: Option<String>,
) -> Option<String> {
    let chat_context = if raw {
        let text = render_raw(events.iter().copied());
        (!text.is_empty()).then_some(text)
    } else {
        let text = render_digest(events.iter().copied(), max_items);
        (!text.is_empty()).then_some(text)
    };
    let mut sections = Vec::new();
    if let Some(context) = chat_context {
        sections.push(context);
    }
    if let Some(context) = work_context {
        sections.push(context);
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn should_wait(scanned_count: usize, wait_ms: u64) -> bool {
    scanned_count == 0 && wait_ms > 0
}

fn continuation(
    context: Option<String>,
    pending: bool,
    full_page: bool,
    runtime_client: Option<&str>,
) -> Option<String> {
    if !pending && !full_page {
        return context;
    }
    let status = if pending {
        "More unread messages remain."
    } else {
        "The scan page was full; more unread records may remain."
    };
    // Stdin-only hook identity is not inherited by a later poll. State the
    // supported explicit environment binding as data, never shell-interpolate it.
    let next = match runtime_client {
        Some(client) => format!(
            "Continue in the same AIRC scope with airc codex-hook poll, setting AIRC_CLIENT_ID to the JSON string value {}.", serde_json::json!(client)
        ),
        None => "Continue by invoking the same hook with the same scope and identity context (consumer codex-hook:default).".to_string(),
    };
    let hint = format!("{status} {next}");
    Some(match context {
        Some(context) => format!("{context}\n{hint}"),
        None => hint,
    })
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

/// Persist the empty origin before waiting, then read durable catch-up.
async fn poll_events_with_wake<W>(
    airc: &Airc,
    consumer_id: &str,
    filter: EventFilter,
    count: usize,
    wait_ms: u64,
    wake: W,
) -> Result<(EventScan, usize), Box<dyn std::error::Error>>
where
    W: std::future::Future<Output = Result<Vec<TranscriptEvent>, Box<dyn std::error::Error>>>,
{
    let (batch, scanned_count) = unread_events(airc, consumer_id, filter.clone(), count).await?;
    if should_wait(scanned_count, wait_ms) {
        // Establish an empty consumer's origin before polling the wake future.
        // A burst during the wait must be read forward, never bootstrapped as a tail.
        if let Some(cursor) = &batch.scanned_through {
            airc.save_runtime_scan_cursor(consumer_id, cursor).await?;
        }
        return catch_up_after_wake(wake, || unread_events(airc, consumer_id, filter, count)).await;
    }
    Ok((batch, scanned_count))
}

async fn unread_events(
    airc: &Airc,
    consumer_id: &str,
    filter: EventFilter,
    count: usize,
) -> Result<(EventScan, usize), Box<dyn std::error::Error>> {
    let previous = airc.load_runtime_cursor(consumer_id).await?;
    let Some(through) = airc.latest_subscribed_cursor().await? else {
        return Ok((
            EventScan {
                events: vec![],
                scanned_through: Some(previous.unwrap_or(TranscriptCursor {
                    lamport: 0,
                    event_id: airc_core::EventId::from_u128(0),
                })),
            },
            0,
        ));
    };
    let batch = airc
        .scan_agent_feed(previous.as_ref(), &through, count.max(1))
        .await?;
    let more = batch
        .scanned_through
        .as_ref()
        .is_some_and(|c| (c.lamport, c.event_id.0) < (through.lamport, through.event_id.0));
    let progressed = batch.scanned_through.as_ref() != previous.as_ref();
    let (batch, matches) = filter_page(batch, filter);
    // Preserve continuation on byte-short pages and avoid waiting before saving scan progress.
    let scanned_count = if more {
        count.max(1)
    } else if progressed {
        matches.max(1)
    } else {
        matches
    };
    Ok((batch, scanned_count))
}

fn filter_page(mut batch: EventScan, filter: EventFilter) -> (EventScan, usize) {
    // Count the bounded raw page before filtering: matching event count cannot
    // tell whether an excluded or mixed page filled the scan budget.
    let scanned_count = batch.events.len();
    batch.events.retain(|event| filter.matches(event));
    (batch, scanned_count)
}

/// Stop before the first visible message that cannot be emitted this turn.
/// Deliberately suppressed lifecycle/self records do not spend the message budget.
fn delivery_end(
    events: &[TranscriptEvent],
    max_items: usize,
    raw: bool,
    visible: impl Fn(&TranscriptEvent) -> bool,
) -> usize {
    let mut shown = 0;
    for (index, event) in events.iter().enumerate() {
        if visible(event)
            && (raw || (!is_work_queue_event(event) && !is_runtime_alive_event(event)))
        {
            if !raw && shown == max_items.max(1) {
                return index;
            }
            shown += 1;
        }
    }
    events.len()
}

fn delivered_cursor(batch: &EventScan, end: usize) -> Option<TranscriptCursor> {
    if end == batch.events.len() {
        batch.scanned_through.clone()
    } else {
        end.checked_sub(1).map(|index| batch.events[index].cursor())
    }
}

/// Subscription delivery only wakes the reader; the durable scan owns ordering.
async fn catch_up_after_wake<W, F, R, T>(wake: W, read: F) -> Result<T, Box<dyn std::error::Error>>
where
    W: std::future::Future<Output = Result<Vec<TranscriptEvent>, Box<dyn std::error::Error>>>,
    F: FnOnce() -> R,
    R: std::future::Future<Output = Result<T, Box<dyn std::error::Error>>>,
{
    let _ = wake.await?;
    read().await
}

async fn wait_for_one_event(
    airc: &Airc,
    _filter: EventFilter,
    wait_ms: u64,
) -> Result<Vec<TranscriptEvent>, Box<dyn std::error::Error>> {
    let mut stream = airc.subscribe_agent_feed().await?;
    match tokio::time::timeout(Duration::from_millis(wait_ms), stream.next()).await {
        Ok(Some(Ok(event))) => Ok(vec![event.as_ref().clone()]),
        Ok(Some(Err(LiveLag { .. }))) | Ok(None) | Err(_) => Ok(Vec::new()),
    }
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
fn drain_stdin() -> Result<Option<String>, Box<dyn std::error::Error>> {
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
            return Ok(None);
        }
    };

    if raw.trim().is_empty() {
        return Ok(None);
    }
    // PostToolUse may include a large tool response. Validate the JSON while
    // skipping unknown fields instead of building a DOM for the entire payload.
    #[derive(Deserialize)]
    struct HookInput {
        session_id: Option<String>,
    }
    if !raw.trim_start().starts_with('{') {
        return Err("Codex hook stdin must be a JSON object".into());
    }
    let input: HookInput = serde_json::from_str(&raw)?;
    Ok(input
        .session_id
        .filter(|session| !session.trim().is_empty())
        .map(|session| format!("codex:{session}")))
}

fn print_hook_payload(context: &str, event_name: &str) -> Result<(), Box<dyn std::error::Error>> {
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
            hook_event_name: event_name,
            additional_context: context,
        },
    };
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &payload)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

fn render_raw<'a>(events: impl IntoIterator<Item = &'a TranscriptEvent>) -> String {
    events
        .into_iter()
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

fn render_digest<'a>(
    events: impl IntoIterator<Item = &'a TranscriptEvent>,
    max_items: usize,
) -> String {
    let messages = digest_messages(events);
    if messages.is_empty() {
        return String::new();
    }

    let max_items = max_items.max(1);
    let hidden = messages.len().saturating_sub(max_items);
    let shown = &messages[..messages.len().min(max_items)];
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
            "first {} shown; {hidden} remain pending",
            shown.len()
        ));
    }
    for message in shown {
        lines.push(format!("- {}: {}", message.peer, message.body));
    }
    if hidden > 0 {
        lines.push("continue: airc codex-hook poll".to_string());
    }
    lines.join("\n")
}

#[derive(Clone)]
struct DigestMessage {
    peer: String,
    body: String,
}

fn digest_messages<'a>(
    events: impl IntoIterator<Item = &'a TranscriptEvent>,
) -> Vec<DigestMessage> {
    let mut messages = Vec::new();
    for event in events {
        if is_work_queue_event(event) || is_runtime_alive_event(event) {
            continue;
        }
        let peer = event.peer_id.to_string();
        let body = summarize_body(event);
        messages.push(DigestMessage { peer, body });
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

#[cfg(test)]
mod tests {
    use airc_core::{
        Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
        TranscriptKind,
    };
    use serde_json::json;

    use super::{
        catch_up_after_wake, consumer_id, delivered_cursor, delivery_end, render_digest, render_raw,
    };
    use airc_lib::EventScan;

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

    // Regression: 44 fetched messages with an eight-message budget must all be
    // delivered in order, including identical bodies, without acknowledging the tail.
    #[test]
    fn digest_checkpoint_preserves_every_pending_message_and_unicode_body() {
        let mut events: Vec<_> = (1..=44)
            .map(|n| {
                let mut event = event(Body::text(format!(
                    "message-{n}: {} final instruction",
                    "界".repeat(50)
                )));
                event.lamport = n;
                event
            })
            .collect();
        events[1].body = events[0].body.clone();
        let mut delivered = Vec::new();
        let mut offset = 0;
        while offset < events.len() {
            let batch = EventScan {
                events: events[offset..].to_vec(),
                scanned_through: events.last().map(TranscriptEvent::cursor),
            };
            let end = delivery_end(&batch.events, 8, false, |_| true);
            assert_eq!(end, (44 - offset).min(8));
            let output = render_digest(&batch.events[..end], 8);
            for item in &batch.events[..end] {
                assert!(
                    output.contains(&super::summarize_body(item)),
                    "full UTF-8 body retained"
                );
                delivered.push(item.event_id);
            }
            assert_eq!(
                delivered_cursor(&batch, end),
                Some(batch.events[end - 1].cursor())
            );
            offset += end;
        }
        assert_eq!(
            delivered,
            events.iter().map(|e| e.event_id).collect::<Vec<_>>()
        );
    }

    // Regression: an excluded raw page has a checkpoint even with zero messages;
    // a partial visible page must not advance to its raw high-water mark.
    #[test]
    fn excluded_pages_progress_but_unrendered_messages_do_not() {
        let mut excluded = event(Body::text("excluded record"));
        excluded.kind = TranscriptKind::Presence;
        excluded.lamport = 50;
        let batch = EventScan {
            events: Vec::new(),
            scanned_through: Some(excluded.cursor()),
        };
        assert_eq!(
            delivered_cursor(&batch, delivery_end(&batch.events, 8, false, |_| true)),
            Some(excluded.cursor())
        );
        let mut first = event(Body::text("first"));
        first.lamport = 51;
        let mut second = event(Body::text("second"));
        second.lamport = 52;
        let mut tail = excluded.clone();
        tail.lamport = 53;
        let batch = EventScan {
            events: vec![first.clone(), second],
            scanned_through: Some(tail.cursor()),
        };
        let end = delivery_end(&batch.events, 1, false, |_| true);
        assert_eq!(delivered_cursor(&batch, end), Some(first.cursor()));
        assert_eq!(delivered_cursor(&batch, 2), Some(tail.cursor()));
    }

    // Regression: a live notification after the subscribe gap cannot replace an
    // earlier durable message; timeout/no-notification also performs durable catchup.
    #[tokio::test]
    async fn live_wake_always_reads_the_durable_gap() {
        for notified in [false, true] {
            let mut gap = event(Body::text("arrived before subscribe"));
            gap.lamport = 10;
            let mut live = event(Body::text("live notification"));
            live.lamport = 11;
            let wake = if notified {
                vec![live.clone()]
            } else {
                Vec::new()
            };
            let expected = vec![gap.event_id, live.event_id];
            let batch = catch_up_after_wake(async { Ok(wake) }, || async {
                Ok(EventScan {
                    scanned_through: Some(live.cursor()),
                    events: vec![gap, live],
                })
            })
            .await
            .unwrap();
            assert_eq!(
                batch.events.iter().map(|e| e.event_id).collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn full_raw_pages_continue_even_without_visible_context() {
        for context in [None, Some("all matching messages delivered".to_string())] {
            let output = super::continuation(context, false, true, Some("codex:stdin-only"))
                .expect("full raw page needs a continuation");
            assert!(output.contains("may remain"));
            assert!(output.contains("AIRC_CLIENT_ID"));
            assert!(output.contains("codex:stdin-only"));
            assert!(!output.contains("More unread messages remain"));
        }
        for mixed in [false, true] {
            let mut excluded = event(Body::text("excluded"));
            excluded.kind = TranscriptKind::Presence;
            let second = if mixed {
                event(Body::text("visible"))
            } else {
                excluded.clone()
            };
            let raw = EventScan {
                scanned_through: Some(second.cursor()),
                events: vec![excluded, second],
            };
            let (filtered, scanned_count) = super::filter_page(raw, super::hook_filter());
            assert_eq!(scanned_count, 2);
            assert_eq!(filtered.events.len(), usize::from(mixed));
            let end = delivery_end(&filtered.events, 8, false, |_| true);
            assert!(super::continuation(
                None,
                end < filtered.events.len(),
                scanned_count >= 2,
                None
            )
            .is_some());
        }
        assert!(super::continuation(None, false, false, None).is_none());
        assert!(super::continuation(None, true, false, None)
            .unwrap()
            .contains("same hook"));
        assert!(
            !super::should_wait(50, 30_000),
            "excluded full page made progress"
        );
        assert!(
            !super::should_wait(1, 30_000),
            "partial raw page made progress"
        );
        assert!(super::should_wait(0, 30_000));
        assert!(!super::should_wait(0, 0));
    }

    #[tokio::test]
    async fn empty_bootstrap_wait_preserves_the_entire_burst_larger_than_a_page() {
        use airc_core::TranscriptCursor;
        use airc_lib::subscriptions::{self, ChannelName, MeshIdentity, SubscriptionSet};
        use airc_lib::Airc;
        let dir = tempfile::TempDir::new().unwrap();
        let airc = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        let mut set = SubscriptionSet::empty();
        let channel = ChannelName::new("empty-bootstrap").unwrap();
        let room = set
            .subscribe_with_wire_root(dir.path(), &MeshIdentity::new("test"), channel.clone())
            .unwrap()
            .room_id;
        set.set_default(channel).unwrap();
        subscriptions::save(airc.coordinator_store_for_test(), &set)
            .await
            .unwrap();
        assert!(airc.latest_subscribed_cursor().await.unwrap().is_none());
        let origin = TranscriptCursor {
            lamport: 0,
            event_id: EventId::from_u128(0),
        };
        // The real poll path persists origin before it polls this deterministic
        // wake future. Populate the real durable store while the consumer waits.
        let wake = async {
            assert_eq!(
                airc.load_runtime_cursor("burst").await.unwrap(),
                Some(origin)
            );
            for n in 1..=9 {
                let mut e = event(Body::text(format!("burst-{n}")));
                e.room_id = room;
                e.lamport = n;
                e.event_id = EventId::from_u128(n as u128);
                airc.append_event(e).await.unwrap();
            }
            Ok(Vec::new()) // timeout also must catch up, not switch to tail
        };
        let (mut page, _) =
            super::poll_events_with_wake(&airc, "burst", super::hook_filter(), 3, 1, wake)
                .await
                .unwrap();
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.extend(page.events.iter().map(|e| e.lamport));
            let cursor = super::delivered_cursor(&page, page.events.len()).unwrap();
            airc.save_runtime_scan_cursor("burst", &cursor)
                .await
                .unwrap();
            page = super::unread_events(&airc, "burst", super::hook_filter(), 3)
                .await
                .unwrap()
                .0;
        }
        assert_eq!(seen, (1..=9).collect::<Vec<_>>());
        assert!(page.events.is_empty());
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
