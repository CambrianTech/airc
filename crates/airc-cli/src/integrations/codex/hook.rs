//! `airc codex-hook ...` handlers.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use airc_core::{Body, TranscriptEvent, TranscriptKind};
use airc_lib::{Airc, EventFilter, LiveLag};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::client_id::{
    agent_process_client_id, current_client_id, explicit_client_id, RuntimeSelfFilter,
};
use crate::work_suggestions::{is_work_queue_event, render_claimable_work};

const CONSUMER_PREFIX: &str = "codex-hook";
// Automatic context is a notification, not an export of nested transcripts. Keep
// large bodies in durable history; explicit --raw reads retain the full payload.
// 4 KiB is a chosen per-message notification budget, not a transport limit.
const DIGEST_BODY_BYTES: usize = 4096;

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
    let mut turn = airc
        .prepare_agent_delivery(&consumer_id, count.max(1), max_items.max(1))
        .await?;
    turn.retain(&filter);
    let self_filter = RuntimeSelfFilter::new(airc.client_id(), runtime_client.as_deref());
    let (live_end, backlog_end) = delivery_ends(&turn, raw, |e| {
        include_self || !self_filter.is_self_event(e)
    });
    let events = turn.live_events()[..live_end]
        .iter()
        .chain(&turn.backlog_events()[..backlog_end])
        .cloned()
        .collect::<Vec<_>>();

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
        None => work_context(&airc, &events).await?,
    };
    let context = delivery_context(&turn, live_end, backlog_end, raw, work_context, |e| {
        include_self || !self_filter.is_self_event(e)
    });
    if let Some(context) = continuation(
        context,
        live_end < turn.live_events().len() || backlog_end < turn.backlog_events().len(),
        turn.has_pending(live_end, backlog_end),
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
    turn.acknowledge(live_end, backlog_end).await?;

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

    let mut turn = airc
        .prepare_agent_delivery(&consumer_id, count.max(1), max_items.max(1))
        .await?;
    turn.retain(&filter);
    if turn.is_empty() && !turn.has_pending(0, 0) && wait_ms > 0 {
        turn.acknowledge(0, 0).await?; // release ownership before an idle wait
        let _ = wait_for_one_event(&airc, filter.clone(), wait_ms).await?;
        turn = airc
            .prepare_agent_delivery(&consumer_id, count.max(1), max_items.max(1))
            .await?;
        turn.retain(&filter);
    }
    let self_filter = RuntimeSelfFilter::new(airc.client_id(), runtime_client.as_deref());
    let (live_end, backlog_end) = delivery_ends(&turn, raw, |e| {
        include_self || !self_filter.is_self_event(e)
    });
    let events = turn.live_events()[..live_end]
        .iter()
        .chain(&turn.backlog_events()[..backlog_end])
        .cloned()
        .collect::<Vec<_>>();
    let work = work_context(&airc, &events).await?;
    let context = delivery_context(&turn, live_end, backlog_end, raw, work, |e| {
        include_self || !self_filter.is_self_event(e)
    });
    if let Some(context) = continuation(
        context,
        live_end < turn.live_events().len() || backlog_end < turn.backlog_events().len(),
        turn.has_pending(live_end, backlog_end),
        runtime_client.as_deref(),
    ) {
        let mut output = std::io::stdout().lock();
        writeln!(output, "{context}")?;
        output.flush()?;
    }
    turn.acknowledge(live_end, backlog_end).await?;

    Ok(())
}

fn delivery_ends(
    turn: &airc_lib::AgentDeliveryTurn,
    raw: bool,
    visible: impl Fn(&TranscriptEvent) -> bool,
) -> (usize, usize) {
    fn end(
        events: &[TranscriptEvent],
        budget: usize,
        raw: bool,
        visible: &impl Fn(&TranscriptEvent) -> bool,
    ) -> usize {
        if budget == 0 {
            return 0;
        }
        let mut shown = 0;
        for (i, e) in events.iter().enumerate() {
            if visible(e) && (raw || (!is_work_queue_event(e) && !is_routine_liveness_event(e))) {
                if shown == budget {
                    return i;
                }
                shown += 1;
            }
        }
        events.len()
    }
    (
        end(turn.live_events(), turn.live_budget, raw, &visible),
        end(turn.backlog_events(), turn.backlog_budget, raw, &visible),
    )
}
fn delivery_context(
    turn: &airc_lib::AgentDeliveryTurn,
    live_end: usize,
    backlog_end: usize,
    raw: bool,
    work: Option<String>,
    visible: impl Fn(&TranscriptEvent) -> bool,
) -> Option<String> {
    let live: Vec<_> = turn.live_events()[..live_end]
        .iter()
        .filter(|e| visible(e))
        .collect();
    let backlog: Vec<_> = turn.backlog_events()[..backlog_end]
        .iter()
        .filter(|e| visible(e))
        .collect();
    let mut sections = Vec::new();
    if let Some(text) = render_context(&live, turn.live_budget.max(1), raw, work) {
        sections.push(text);
    }
    if let Some(text) = render_context(&backlog, turn.backlog_budget.max(1), raw, None) {
        sections.push(format!(
            "Historical catch-up (retained unread history):\n{text}"
        ));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
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
        if is_work_queue_event(event) || is_routine_liveness_event(event) {
            continue;
        }
        let peer = event.peer_id.to_string();
        let full = summarize_body(event);
        let body = if full.len() <= DIGEST_BODY_BYTES {
            full
        } else {
            format!(
                "[body retained: {} bytes; event {}; room {}; cursor {} / {}] Read in this same AIRC home with `airc inbox --room {} --since-lamport {} --since-event-id 00000000-0000-0000-0000-000000000000 --json`; page forward to this event ID if needed. This is a reference, not the message body.",
                full.len(), event.event_id, event.room_id, event.lamport, event.event_id,
                event.room_id, event.lamport.saturating_sub(1),
            )
        };
        messages.push(DigestMessage { peer, body });
    }
    messages
}

fn is_routine_liveness_event(event: &TranscriptEvent) -> bool {
    // Lease renewals are liveness, not agent correspondence. Keep the event in
    // the durable transcript/raw output, but do not spend the digest budget on it.
    if event
        .headers
        .get(airc_lib::HEADER_FORGE_WORK_EVENT_KIND)
        .map(String::as_str)
        == Some("claim_heartbeat")
    {
        return true;
    }
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

    use super::{consumer_id, delivery_context, delivery_ends, render_digest, render_raw};
    use airc_lib::Airc;

    #[test]
    fn lease_renewals_do_not_bury_peer_messages_and_remain_in_raw_output() {
        let mut renewal = event(Body::Json(json!({"kind":"claim_heartbeat"})));
        renewal.headers.insert(
            airc_lib::HEADER_FORGE_WORK_EVENT_KIND.into(),
            "claim_heartbeat".into(),
        );
        let events = vec![renewal, event(Body::text("review the current head"))];
        let digest = render_digest(&events, 1);
        assert!(digest.contains("review the current head"));
        assert!(!digest.contains("claim_heartbeat"));
        assert!(render_raw(&events).contains("claim_heartbeat"));
        // Ordinary messages discussing a renewal are still correspondence.
        assert!(
            render_digest(&[event(Body::text("claim_heartbeat failed"))], 1)
                .contains("claim_heartbeat failed")
        );
    }
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

    #[test]
    fn oversized_transcript_is_recoverable_without_burying_the_next_request() {
        let huge = "界".repeat(30_000);
        let retained = event(Body::text(&huge));
        let id = retained.event_id.to_string();
        let events = vec![retained, event(Body::text("review the new head"))];
        let digest = render_digest(&events, 8);
        assert!(digest.len() < super::DIGEST_BODY_BYTES);
        assert!(digest.contains(&id));
        assert!(digest.contains("airc inbox --room"));
        assert!(digest.contains("review the new head"));
        assert!(!digest.contains(&huge));
        assert!(
            render_raw(&events).contains(&huge),
            "explicit recovery remains whole"
        );
    }

    async fn fixture() -> (tempfile::TempDir, Airc, RoomId) {
        use airc_lib::subscriptions::{self, ChannelName, MeshIdentity, SubscriptionSet};
        let dir = tempfile::TempDir::new().unwrap();
        let airc = Airc::open_with_wire_root_for_test(dir.path(), dir.path())
            .await
            .unwrap();
        let mut set = SubscriptionSet::empty();
        let channel = ChannelName::new("hook-test").unwrap();
        let room = set
            .subscribe_with_wire_root(dir.path(), &MeshIdentity::new("test"), channel.clone())
            .unwrap()
            .room_id;
        set.set_default(channel).unwrap();
        subscriptions::save(airc.coordinator_store_for_test(), &set)
            .await
            .unwrap();
        (dir, airc, room)
    }
    async fn append(
        airc: &Airc,
        room: RoomId,
        n: u64,
        body: Body,
        self_event: bool,
        excluded: bool,
    ) {
        let mut e = event(body);
        e.room_id = room;
        e.lamport = n;
        e.event_id = EventId::from_u128(n as u128);
        if self_event {
            e.client_id = airc.client_id();
        }
        if excluded {
            e.kind = TranscriptKind::Presence;
        }
        airc.append_event(e).await.unwrap();
    }
    #[tokio::test]
    async fn lease_renewal_does_not_spend_delivery_budget_or_reset_the_cursor() {
        let (_dir, airc, room) = fixture().await;
        drop(
            airc.prepare_agent_delivery("lease-test", 50, 1)
                .await
                .unwrap(),
        );
        let mut renewal = event(Body::Json(json!({"kind":"claim_heartbeat"})));
        renewal.room_id = room;
        renewal.lamport = 1;
        renewal.headers.insert(
            airc_lib::HEADER_FORGE_WORK_EVENT_KIND.into(),
            "claim_heartbeat".into(),
        );
        airc.append_event(renewal).await.unwrap();
        append(&airc, room, 2, Body::text("urgent review"), false, false).await;
        let turn = airc
            .prepare_agent_delivery("lease-test", 50, 1)
            .await
            .unwrap();
        let ends = delivery_ends(&turn, false, |_| true);
        assert_eq!(ends.0, 2);
        assert_eq!(delivery_ends(&turn, true, |_| true).0, 1);
        let context = delivery_context(&turn, ends.0, ends.1, false, None, |_| true).unwrap();
        assert!(context.contains("urgent review"));
        assert!(!context.contains("claim_heartbeat"));
        turn.acknowledge(ends.0, ends.1).await.unwrap();
        let next = airc
            .prepare_agent_delivery("lease-test", 50, 1)
            .await
            .unwrap();
        assert!(next.is_empty());
    }
    #[tokio::test]
    async fn production_dual_turn_preserves_prefixes_self_filter_and_output_failure() {
        let (_dir, airc, room) = fixture().await;
        for n in 1..=44 {
            // Identical bodies remain independently acknowledged by ID; UTF-8 is intact.
            append(
                &airc,
                room,
                n,
                Body::text(format!("{} final instruction", "界".repeat(50))),
                false,
                false,
            )
            .await;
        }
        drop(airc.prepare_agent_delivery("hook", 50, 3).await.unwrap());
        append(&airc, room, 45, Body::text("hidden self"), true, false).await;
        append(
            &airc,
            room,
            46,
            Body::text("excluded presence"),
            false,
            true,
        )
        .await;
        append(
            &airc,
            room,
            47,
            Body::Json(json!({"kind":"alive"})),
            false,
            false,
        )
        .await;
        append(&airc, room, 48, Body::text("fresh-first"), false, false).await;
        append(&airc, room, 49, Body::text("fresh-second"), false, false).await;
        append(&airc, room, 50, Body::text("fresh-pending"), false, false).await;
        let visible = |e: &TranscriptEvent| e.client_id != airc.client_id();
        let mut turn = airc.prepare_agent_delivery("hook", 50, 3).await.unwrap();
        turn.retain(&super::hook_filter());
        let ends = delivery_ends(&turn, false, visible);
        assert_eq!((ends.0, ends.1), (4, 1)); // self/alive consume no budget; presence filtered
        let context = delivery_context(&turn, ends.0, ends.1, false, None, visible).unwrap();
        assert!(
            context.find("fresh-first").unwrap() < context.find("Historical catch-up").unwrap()
        );
        assert!(!context.contains("fresh-pending"));
        assert!(!context.contains("hidden self"));
        assert!(!context.contains("excluded presence"));
        drop(turn); // output failure: no acknowledgment
        let mut retry = airc.prepare_agent_delivery("hook", 50, 3).await.unwrap();
        retry.retain(&super::hook_filter());
        assert_eq!(delivery_ends(&retry, false, visible), ends);
        assert_eq!(
            delivery_context(&retry, ends.0, ends.1, false, None, visible).unwrap(),
            context
        );
        let mut seen = vec![retry.backlog_events()[0].event_id];
        retry.acknowledge(ends.0, ends.1).await.unwrap();
        for _ in 0..50 {
            let mut page = airc.prepare_agent_delivery("hook", 50, 8).await.unwrap();
            page.retain(&super::hook_filter());
            let (l, b) = delivery_ends(&page, false, visible);
            let output = delivery_context(&page, l, b, false, None, visible).unwrap_or_default();
            assert!(!output.contains("fresh-first"));
            for item in &page.backlog_events()[..b] {
                assert!(output.contains(&super::summarize_body(item)));
                seen.push(item.event_id);
            }
            let done = !page.has_pending(l, b);
            page.acknowledge(l, b).await.unwrap();
            if done {
                break;
            }
        }
        assert_eq!(seen, (1..=44).map(EventId::from_u128).collect::<Vec<_>>());
    }
    #[tokio::test]
    async fn production_empty_bootstrap_preserves_burst_larger_than_page() {
        let (_dir, airc, room) = fixture().await;
        let empty = airc.prepare_agent_delivery("burst", 3, 3).await.unwrap();
        assert!(empty.is_empty());
        empty.acknowledge(0, 0).await.unwrap(); // actual pre-wait path
        for n in 1..=9 {
            append(
                &airc,
                room,
                n,
                Body::text(format!("burst-{n}")),
                false,
                false,
            )
            .await;
        }
        let mut seen = Vec::new();
        for _ in 0..3 {
            let page = airc.prepare_agent_delivery("burst", 3, 3).await.unwrap();
            let (l, b) = delivery_ends(&page, false, |_| true);
            assert_eq!(b, 0);
            seen.extend(page.live_events()[..l].iter().map(|e| e.lamport));
            assert!(delivery_context(&page, l, b, false, None, |_| true).is_some());
            page.acknowledge(l, b).await.unwrap();
        }
        assert_eq!(seen, (1..=9).collect::<Vec<_>>());
        assert!(airc
            .prepare_agent_delivery("burst", 3, 3)
            .await
            .unwrap()
            .is_empty());
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
