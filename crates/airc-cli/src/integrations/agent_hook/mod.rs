//! The runtime-neutral half of every agent hook (card b00d2724).
//!
//! A hook turn is the same for every runtime: read the session the runtime names on
//! stdin, resolve this session's delivery consumer, prepare a delivery turn, keep what
//! the [`DeliverySelection`] shows, summarize history from before attachment once,
//! render a digest, print the payload, then acknowledge. A runtime adapter
//! (`integrations::codex`, `integrations::claude`) holds only a [`HookRuntime`]: its
//! verb, its session prefix, and how it re-arms a live reader.

mod args;
mod io;
mod render;
pub mod settings;
#[cfg(test)]
mod tests;

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use airc_core::TranscriptEvent;
use airc_lib::{AgentDeliveryTurn, Airc, EventFilter, LiveLag};
use futures::StreamExt;

use crate::client_id::{agent_process_client_id, current_client_id, explicit_client_id};
use crate::work_suggestions::{is_work_queue_event, render_claimable_work};

pub use args::HookReadArgs;
pub(crate) use render::{history_line, more_addressed_line, original_time};

/// What differs between runtimes. Everything else is this module's.
pub struct HookRuntime {
    /// CLI verb and consumer-id prefix: `codex-hook`, `claude-hook`.
    pub verb: &'static str,
    /// Runtime name for diagnostics.
    pub name: &'static str,
    /// Prefix that turns the hook's stdin `session_id` into a runtime client id.
    pub session_prefix: &'static str,
    /// How this runtime re-arms a live `airc join` reader. `Some` makes every digest
    /// say in one line when no reader for this session is beating; `None` keeps the
    /// digest silent about readers.
    pub rearm_reader: Option<&'static str>,
}

/// The hook events both runtimes deliver context on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    UserPromptSubmit,
    PostToolUse,
}

impl HookEvent {
    fn name(self) -> &'static str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PostToolUse => "PostToolUse",
        }
    }
}

/// Which events a hook shows. THE SEAM for an identity-resolved subscription
/// (card 6fb2ea16: mentions of me, SOS, DMs): that card adds fields here, with
/// `Default` remaining today's behaviour, and no adapter changes. Its contract:
/// an authorized direct request is never filtered out, and acknowledging a turn
/// marks events DELIVERED to this consumer, never that a reader consumed them.
/// "Addressed to me" has one predicate, [`airc_lib::Addressee::is_addressed`]
/// (typed target, or a leading `@name` that is mine); the pre-attachment
/// summary already uses it, and a selection field must use it too.
#[derive(Clone, Debug)]
pub struct DeliverySelection {
    /// Transcript filter applied before budgeting; filtered events still advance.
    pub filter: EventFilter,
    /// Show this runtime's own sends. Default: hidden, still advancing the cursor.
    pub include_self: bool,
}

impl Default for DeliverySelection {
    fn default() -> Self {
        Self {
            filter: EventFilter {
                selection: airc_core::EventSelection::Correspondence,
                ..EventFilter::default()
            },
            include_self: false,
        }
    }
}

impl DeliverySelection {
    /// The self-echo rule this selection applies: the runtime's own sends are
    /// hidden (and still advance the cursor) unless it asked to include them.
    /// ONE rule for live delivery, the addressed history page and the store count.
    fn own_sends<'a>(
        &self,
        rule: &'a airc_core::RuntimeSelf,
    ) -> Option<&'a airc_core::RuntimeSelf> {
        (!self.include_self).then_some(rule)
    }

    fn shows(&self, event: &TranscriptEvent, rule: &airc_core::RuntimeSelf) -> bool {
        !self
            .own_sends(rule)
            .is_some_and(|own| own.is_self_event(event))
    }
}

/// How much one hook call reads and shows.
pub struct HookOptions {
    /// Maximum unread events to scan.
    pub count: usize,
    /// Maximum events to show.
    pub max_items: usize,
    /// Raw lines instead of a digest.
    pub raw: bool,
    pub selection: DeliverySelection,
}

pub async fn run_hook(
    runtime: &HookRuntime,
    home: &Path,
    options: &HookOptions,
    event: HookEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    let hook_session = io::drain_stdin(runtime)?;
    let airc = crate::commands::attached_airc(home).await?;
    // An identity the runtime states outright wins; then the hook's own session, which
    // names this exact task; only then the ancestor-process guess. The guess ranked
    // above the session made every hook call under one agent ancestor share a cursor (on
    // a dev box run from Claude Code, two Codex sessions read as one).
    let runtime_client = match explicit_client_id().or(hook_session) {
        Some(client) => Some(client),
        None => agent_process_client_id()?,
    };
    let consumer = consumer_id(runtime, runtime_client.as_deref());
    let mut turn = prepare(&airc, &consumer, options).await?;
    let resume =
        match crate::agent_resume::prepare(&airc, &consumer, event == HookEvent::PostToolUse).await
        {
            Ok(delivery) => delivery,
            Err(error) => {
                eprintln!("airc: resume context unavailable: {error}");
                None
            }
        };
    let resume_text = resume.as_ref().map(|delivery| delivery.text.clone());
    let (context, live_end) = compose(
        runtime,
        &airc,
        &mut turn,
        runtime_client.as_deref(),
        options,
        resume_text,
    )
    .await?;
    if let Some(context) = context {
        io::print_hook_payload(&context, event.name())?;
        std::io::stdout().flush()?;
        if let Some(delivery) = resume {
            if let Err(error) = crate::agent_resume::acknowledge(&airc, delivery).await {
                eprintln!("airc: resume acknowledgment failed; context remains due: {error}");
            }
        }
    }
    // Output is flushed (or there was nothing to show): the history summary is settled.
    turn.acknowledge(live_end, true).await?;
    Ok(())
}

pub async fn run_poll(
    runtime: &HookRuntime,
    home: &Path,
    options: &HookOptions,
    wait_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let runtime_client = current_client_id()?;
    let consumer = consumer_id(runtime, runtime_client.as_deref());
    let mut turn = prepare(&airc, &consumer, options).await?;
    if turn.is_empty() && !turn.has_pending(0) && !turn.has_history() && wait_ms > 0 {
        turn.acknowledge(0, false).await?; // release ownership before an idle wait
        wait_for_one_event(&airc, wait_ms).await?;
        turn = prepare(&airc, &consumer, options).await?;
    }
    let (context, live_end) = compose(
        runtime,
        &airc,
        &mut turn,
        runtime_client.as_deref(),
        options,
        None,
    )
    .await?;
    if let Some(context) = context {
        let mut output = std::io::stdout().lock();
        writeln!(output, "{context}")?;
        output.flush()?;
    }
    turn.acknowledge(live_end, true).await?;
    Ok(())
}

async fn prepare(
    airc: &Airc,
    consumer: &str,
    options: &HookOptions,
) -> Result<AgentDeliveryTurn, Box<dyn std::error::Error>> {
    let mut turn = airc
        .prepare_agent_delivery(consumer, options.count.max(1), options.max_items.max(1))
        .await?;
    turn.retain(&options.selection.filter);
    Ok(turn)
}

/// The context one turn shows, and the live prefix it covers.
async fn compose(
    runtime: &HookRuntime,
    airc: &Airc,
    turn: &mut AgentDeliveryTurn,
    runtime_client: Option<&str>,
    options: &HookOptions,
    resume: Option<String>,
) -> Result<(Option<String>, usize), Box<dyn std::error::Error>> {
    let self_filter = airc_core::RuntimeSelf::new(airc.client_id(), runtime_client);
    let shows = |e: &TranscriptEvent| options.selection.shows(e, &self_filter);
    let live_end = delivery_end(turn, options.raw, shows);
    let me = airc.addressee().await?;
    let window = airc_lib::AddressedWindow {
        // Addressee candidates (store-selected, never ambient traffic) one turn
        // may read: the hook's own scan size (`--count`).
        candidates: options.count.max(1),
        page: turn.budget,
    };
    let history = turn
        .history_summary(
            options.selection.filter.clone(),
            options.selection.own_sends(&self_filter),
            |e| {
                options.selection.filter.matches(e) && (options.raw || render::is_correspondence(e))
            },
            &me,
            window,
        )
        .await?;
    let work = match resume {
        Some(text) => Some(text),
        None => work_context(airc, &turn.live_events()[..live_end]).await?,
    };
    let mut context = delivery_context(
        turn,
        live_end,
        options.raw,
        work,
        history.as_ref(),
        runtime.verb,
        shows,
    );
    if context.is_some() {
        if let Some(line) = reader_line(runtime, airc, runtime_client).await {
            context = context.map(|text| format!("{text}\n{line}"));
        }
    }
    let context = continuation(runtime, context, turn.has_pending(live_end), runtime_client);
    Ok((context, live_end))
}

fn delivery_end(
    turn: &AgentDeliveryTurn,
    raw: bool,
    visible: impl Fn(&TranscriptEvent) -> bool,
) -> usize {
    let events = turn.live_events();
    let mut shown = 0;
    for (i, e) in events.iter().enumerate() {
        if visible(e) && (raw || render::is_correspondence(e)) {
            if shown == turn.budget {
                return i;
            }
            shown += 1;
        }
    }
    events.len()
}

fn delivery_context(
    turn: &AgentDeliveryTurn,
    live_end: usize,
    raw: bool,
    work: Option<String>,
    history: Option<&airc_lib::HistorySummary>,
    verb: &str,
    visible: impl Fn(&TranscriptEvent) -> bool,
) -> Option<String> {
    let live: Vec<_> = turn.live_events()[..live_end]
        .iter()
        .filter(|e| visible(e))
        .collect();
    let mut sections = Vec::new();
    if let Some(text) = history.and_then(render::history_section) {
        sections.push(text);
    }
    let chat = if raw {
        render::render_raw(live.iter().copied())
    } else {
        render::render_digest(live.iter().copied(), turn.budget, verb)
    };
    if !chat.is_empty() {
        sections.push(chat);
    }
    if let Some(work) = work {
        sections.push(work);
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

/// One line when this session has no live `airc join` reader, judged from presence
/// beats (a join beats as its runtime client). Says when it cannot tell.
async fn reader_line(
    runtime: &HookRuntime,
    airc: &Airc,
    runtime_client: Option<&str>,
) -> Option<String> {
    let rearm = runtime.rearm_reader?;
    let Some(client) = runtime_client else {
        return Some(format!(
            "AIRC: cannot tell whether a live `airc join` reader is attached: this hook has no session identity. {rearm}"
        ));
    };
    match airc
        .active_agents(airc_lib::agent_heartbeat::PRESENCE_TTL, 0)
        .await
    {
        Ok(agents) => {
            let attached = agents.iter().any(|agent| {
                agent.peer == airc.peer_id() && agent.client_id.as_deref() == Some(client)
            });
            (!attached).then(|| {
                format!(
                    "AIRC: no live `airc join` reader for {client} (no presence beat in {} s); between hook calls this session hears nothing. {rearm}",
                    airc_lib::agent_heartbeat::PRESENCE_TTL.as_secs()
                )
            })
        }
        Err(error) => Some(format!(
            "AIRC: cannot tell whether a live `airc join` reader is attached ({error}). {rearm}"
        )),
    }
}

fn continuation(
    runtime: &HookRuntime,
    context: Option<String>,
    pending: bool,
    runtime_client: Option<&str>,
) -> Option<String> {
    if !pending {
        return context;
    }
    // Stdin-only hook identity is not inherited by a later poll. State the
    // supported explicit environment binding as data, never shell-interpolate it.
    let verb = runtime.verb;
    let next = match runtime_client {
        Some(client) => format!(
            "Continue in the same AIRC scope with airc {verb} poll, setting AIRC_CLIENT_ID to the JSON string value {}.",
            serde_json::json!(client)
        ),
        None => format!(
            "Continue by invoking the same hook with the same scope and identity context (consumer {verb}:default)."
        ),
    };
    let hint = format!("More unread messages remain. {next}");
    Some(match context {
        Some(context) => format!("{context}\n{hint}"),
        None => hint,
    })
}

fn consumer_id(runtime: &HookRuntime, runtime_client: Option<&str>) -> String {
    format!("{}:{}", runtime.verb, runtime_client.unwrap_or("default"))
}

async fn wait_for_one_event(airc: &Airc, wait_ms: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = airc.subscribe_agent_feed().await?;
    // Any outcome ends the wait: an event, a lag, a closed stream or the deadline.
    // The durable turn prepared next is what decides what is delivered.
    let _woke: Result<Option<Result<_, LiveLag>>, _> =
        tokio::time::timeout(Duration::from_millis(wait_ms), stream.next()).await;
    Ok(())
}
