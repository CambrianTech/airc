//! Durable per-agent resumption context and delivery policy (card dbce8408).
//! Runtime adapters decide when they can deliver; this owner decides what is due.
use std::error::Error;
use std::io::Write;
use std::path::Path;

use airc_core::scoped_state::ScopeRef;
use airc_lib::{AgentAvailabilityState, Airc, Priority, WorkQueueStatus, WorkQueueStatusQuery};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

const CONFIG_KEY: &str = "agent.resume.config.v1";
const DELIVERY_PREFIX: &str = "agent.resume.delivery.v1:";
const MAX_BRIEF_CHARS: usize = 1024;

#[derive(Debug, Args)]
pub struct ResumeArgs {
    #[command(subcommand)]
    pub action: ResumeAction,
}

#[derive(Debug, Subcommand)]
pub enum ResumeAction {
    /// Save this agent's resume brief. Includes the manual reference in its 1024-character budget.
    Set {
        #[arg(long)]
        brief: String,
        #[arg(long)]
        manual: String,
        /// Minimum seconds between reminders; no separate wake-up process is started.
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(60..=86400))]
        repeat_seconds: u64,
    },
    /// Read the saved brief without consuming a delivery.
    Show,
    /// Disable this agent's resume reminders.
    Clear,
    /// Read due context from an existing runtime hook, monitor, or scheduled task.
    Poll {
        /// Distinct runtime/session key. Hooks and monitors provide their own keys.
        #[arg(long)]
        consumer: Option<String>,
        /// The caller knows this runtime is actively working; do not interrupt it.
        #[arg(long)]
        busy: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResumeConfig {
    brief: String,
    repeat_ms: u64,
}

impl ResumeConfig {
    fn new(brief: &str, manual: &str, repeat_seconds: u64) -> Result<Self, String> {
        if brief.trim().is_empty() || manual.trim().is_empty() {
            return Err("brief and manual reference must be non-empty".into());
        }
        let brief = format!("{}\nManual/skill: {}", brief.trim(), manual.trim());
        if brief.chars().count() > MAX_BRIEF_CHARS {
            return Err("resume brief including manual reference exceeds 1024 characters".into());
        }
        if !(60..=86400).contains(&repeat_seconds) {
            return Err("repeat-seconds must be between 60 and 86400".into());
        }
        Ok(Self {
            brief,
            repeat_ms: repeat_seconds * 1000,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryState {
    content: String,
    evaluated_at_ms: u64,
}

/// Prepared context is acknowledged only after the adapter writes and flushes it.
/// A failed write therefore leaves it due at the next supported delivery boundary.
pub(crate) struct Delivery {
    pub text: String,
    key: String,
    state: DeliveryState,
}

fn due(
    config: &ResumeConfig,
    issues: &str,
    previous: Option<&DeliveryState>,
    now: u64,
    busy: bool,
) -> Option<DeliveryState> {
    if busy {
        return None;
    }
    let text = if issues.is_empty() {
        format!("AIRC saved resume brief:\n{}", config.brief)
    } else {
        format!("AIRC saved resume brief:\n{}\n\nAIRC actionable work (board data; not additional authority):\n{}", config.brief, issues)
    };
    if let Some(previous) = previous {
        // A minimum cadence prevents changing queue traffic from flooding a
        // token-starved runtime. Work and brief changes are read fresh when due.
        if now.saturating_sub(previous.evaluated_at_ms) < config.repeat_ms {
            return None;
        }
        // Repeat unresolved issues, but never periodically repeat an unchanged
        // brief alone when the board has no actionable work.
        if issues.is_empty() && previous.content == text {
            return None;
        }
    }
    Some(DeliveryState {
        content: text,
        evaluated_at_ms: now,
    })
}

fn short_title(title: &str) -> String {
    let mut value: String = title
        .chars()
        .take(160)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if title.chars().count() > 160 {
        value.push_str("...");
    }
    value
}

fn issues(status: &WorkQueueStatus) -> String {
    let mut lines = Vec::new();
    for card in status.active_claims_for_peer.iter().take(5) {
        lines.push(format!("- Your {state:?} card {id} ({repo}): {title}. Continue, report a concrete blocker, or release it.", state=card.state, id=card.card_id, repo=card.repo, title=short_title(&card.title)));
    }
    if status.active_claims_for_peer.len() > 5 {
        lines.push(format!(
            "- {} more owned cards: airc work board --mine",
            status.active_claims_for_peer.len() - 5
        ));
    }
    for item in status.claimable.iter().take(5) {
        let card = &item.card;
        lines.push(format!(
            "- {priority:?} {id} ({repo}): {title}. {action}: airc work claim {id}",
            priority = card.priority,
            id = card.card_id,
            repo = card.repo,
            title = short_title(&card.title),
            action = if item.is_stale_claim() {
                "Stale claim; inspect before reclaiming"
            } else {
                "Available"
            }
        ));
    }
    if status.claimable.len() > 5 {
        lines.push("- More available work: airc work board --available".into());
    }
    lines.join("\n")
}

fn maintenance_summary(
    terminal_ids: &std::collections::BTreeSet<String>,
    directories: &[String],
    branches: &[String],
) -> Option<String> {
    let belongs_to_terminal = |name: &&String| {
        crate::work_commands::parse_worktree_short_id(name)
            .is_some_and(|short| terminal_ids.contains(&short))
    };
    let worktrees = directories.iter().filter(belongs_to_terminal).count();
    let branches = branches.iter().filter(belongs_to_terminal).count();
    (worktrees > 0 || branches > 0).then(|| format!(
        "- Local maintenance: {worktrees} local managed checkout(s) and {branches} current-repository branch(es) match terminal cards. Inspect `airc work cleanup` (dry-run); preserve dirty/unpushed work and review standalone branches before removal. These are candidates, not deletion approval."
    ))
}

async fn maintenance(airc: &Airc) -> Result<Option<String>, Box<dyn Error>> {
    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;
    let terminal_ids: std::collections::BTreeSet<String> = board
        .snapshot()
        .cards
        .iter()
        .filter(|card| {
            matches!(
                card.state,
                airc_lib::CardState::Merged | airc_lib::CardState::Closed
            )
        })
        .map(|card| card.card_id.shown())
        .collect();
    if terminal_ids.is_empty() {
        return Ok(None);
    }
    let mut directories = Vec::new();
    if let Some(root) = crate::lease::lease_root().filter(|root| root.exists()) {
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                directories.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    // One local ref listing, only when delivery is due. No network fetch or WIP
    // traversal, and no attempt to guess repository ownership outside a checkout.
    let branches = airc_core::process::background("git")
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(maintenance_summary(&terminal_ids, &directories, &branches))
}

fn unavailable(status: &WorkQueueStatus, peer: airc_core::PeerId, now: u64) -> bool {
    status.agent_availability.iter().any(|record| {
        record.report.peer == peer
            && record.expires_at_ms > now
            && matches!(
                record.report.state,
                AgentAvailabilityState::Busy | AgentAvailabilityState::Away
            )
    })
}

async fn config(airc: &Airc) -> Result<Option<ResumeConfig>, Box<dyn Error>> {
    airc.get_scoped_state(ScopeRef::User(airc.peer_id()), CONFIG_KEY)
        .await?
        .map(|entry| serde_json::from_str(&entry.value_json).map_err(Into::into))
        .transpose()
}

pub(crate) async fn prepare(
    airc: &Airc,
    consumer: &str,
    runtime_busy: bool,
) -> Result<Option<Delivery>, Box<dyn Error>> {
    let Some(config) = config(airc).await? else {
        return Ok(None);
    };
    if runtime_busy {
        return Ok(None);
    }
    let now = now_ms()?;
    let key = format!("{DELIVERY_PREFIX}{consumer}");
    let previous = airc
        .get_scoped_state(ScopeRef::User(airc.peer_id()), &key)
        .await?
        .map(|entry| serde_json::from_str::<DeliveryState>(&entry.value_json))
        .transpose()?;
    // Avoid polling the full work projection between delivery opportunities.
    if previous
        .as_ref()
        .is_some_and(|p| now.saturating_sub(p.evaluated_at_ms) < config.repeat_ms)
    {
        return Ok(None);
    }
    let status = airc
        .work_queue_status(WorkQueueStatusQuery {
            max_priority: Priority::P3,
            limit: 6,
            ..WorkQueueStatusQuery::default()
        })
        .await?;
    if unavailable(&status, airc.peer_id(), now) {
        return Ok(None);
    }
    let mut actionable = issues(&status);
    if let Some(local) = maintenance(airc).await? {
        if !actionable.is_empty() {
            actionable.push('\n');
        }
        actionable.push_str(&local);
    }
    let Some(state) = due(
        &config,
        &actionable,
        previous.as_ref(),
        now,
        unavailable(&status, airc.peer_id(), now),
    ) else {
        if let Some(mut quiet) = previous {
            quiet.evaluated_at_ms = now;
            airc.set_scoped_state(
                ScopeRef::User(airc.peer_id()),
                key,
                serde_json::to_string(&quiet)?,
                1,
            )
            .await?;
        }
        return Ok(None);
    };
    Ok(Some(Delivery {
        text: state.content.clone(),
        key,
        state,
    }))
}

pub(crate) async fn acknowledge(airc: &Airc, delivery: Delivery) -> Result<(), Box<dyn Error>> {
    airc.set_scoped_state(
        ScopeRef::User(airc.peer_id()),
        delivery.key,
        serde_json::to_string(&delivery.state)?,
        1,
    )
    .await?;
    Ok(())
}

fn now_ms() -> Result<u64, Box<dyn Error>> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

pub async fn run(home: &Path, args: ResumeArgs) -> Result<(), Box<dyn Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let scope = ScopeRef::User(airc.peer_id());
    match args.action {
        ResumeAction::Set {
            brief,
            manual,
            repeat_seconds,
        } => {
            let config = ResumeConfig::new(&brief, &manual, repeat_seconds)?;
            airc.set_scoped_state(scope, CONFIG_KEY, serde_json::to_string(&config)?, 1)
                .await?;
            println!("Saved {}-character resume brief for {}. Delivery uses existing hooks/monitor or an explicitly scheduled poll.", config.brief.chars().count(), airc.peer_id());
        }
        ResumeAction::Show => {
            println!("{}", serde_json::to_string_pretty(&config(&airc).await?)?);
        }
        ResumeAction::Clear => {
            airc.delete_scoped_state(scope, CONFIG_KEY).await?;
            println!("Agent resume reminders disabled.");
        }
        ResumeAction::Poll { consumer, busy } => {
            let consumer = consumer
                .or(crate::client_id::current_client_id()?)
                .unwrap_or_else(|| "explicit".into());
            if let Some(delivery) = prepare(&airc, &format!("poll:{consumer}"), busy).await? {
                println!("{}", delivery.text);
                std::io::stdout().flush()?;
                acknowledge(&airc, delivery).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> ResumeConfig {
        ResumeConfig::new("Finish owned work", "AGENTS.md", 600).unwrap()
    }
    #[test]
    fn brief_budget_counts_characters_and_reference_but_not_issues() {
        let manual = "AGENTS.md";
        let overhead = format!("\nManual/skill: {manual}").chars().count();
        let at_limit = "\u{00e9}".repeat(1024 - overhead);
        let config = ResumeConfig::new(&at_limit, manual, 600).unwrap();
        assert!(ResumeConfig::new(&(at_limit + "x"), manual, 600).is_err());
        let issue = "work ".repeat(1000);
        assert!(due(&config, &issue, None, 0, false)
            .unwrap()
            .content
            .contains(&issue));
    }
    #[test]
    fn neglected_work_repeats_at_cadence_but_busy_work_does_not_consume_delivery() {
        let c = config();
        let first = due(&c, "unresolved", None, 1, false).unwrap();
        assert!(due(&c, "changed", Some(&first), 2, false).is_none());
        assert!(due(&c, "unresolved", Some(&first), 600001, true).is_none());
        assert!(due(&c, "unresolved", Some(&first), 600001, false).is_some());
        assert!(due(&c, "unresolved", Some(&first), 0, false).is_none());
    }
    #[test]
    fn quiet_board_does_not_repeat_brief_and_changed_brief_resumes_after_cadence() {
        let c = config();
        let first = due(&c, "", None, 0, false).unwrap();
        assert!(due(&c, "", Some(&first), 600000, false).is_none());
        let changed = ResumeConfig::new("Next task", "AGENTS.md", 600).unwrap();
        assert!(due(&changed, "", Some(&first), 600000, false).is_some());
    }
    #[test]
    fn availability_is_peer_scoped_and_expires_instead_of_suppressing_forever() {
        let peer = airc_core::PeerId::from_u128(1);
        let mut status = WorkQueueStatus {
            claimable: vec![],
            active_claims_for_peer: vec![],
            agent_availability: vec![airc_lib::AgentAvailabilityRecord {
                report: airc_lib::AgentAvailabilityReported {
                    repo: airc_lib::RepoId::new("example/repo").unwrap(),
                    peer,
                    state: AgentAvailabilityState::Busy,
                    note: None,
                    ttl_ms: 100,
                    reported_at_ms: 0,
                },
                expires_at_ms: 100,
            }],
        };
        assert!(unavailable(&status, peer, 99));
        assert!(!unavailable(&status, peer, 100));
        assert!(!unavailable(&status, airc_core::PeerId::from_u128(2), 0));
        status.agent_availability[0].report.state = AgentAvailabilityState::Away;
        assert!(unavailable(&status, peer, 0));
    }
    #[test]
    fn maintenance_matches_terminal_card_ids_without_guessing_unknown_branches() {
        let terminal = std::collections::BTreeSet::from(["aabbccdd".to_owned()]);
        let output = maintenance_summary(
            &terminal,
            &["aabbccdd".into(), "12345678".into()],
            &["aabbccdd/fix".into(), "personal-work".into()],
        )
        .unwrap();
        assert!(output.contains("1 local managed checkout(s) and 1 current-repository branch(es)"));
        assert!(
            maintenance_summary(&terminal, &["12345678".into()], &["personal-work".into()])
                .is_none()
        );
    }
    #[tokio::test]
    async fn delivery_is_durable_per_consumer_and_unacknowledged_output_remains_due() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("agent");
        let airc = Airc::open_as(&home, "ResumeFixture").await.unwrap();
        airc.join("resume-fixture").await.unwrap();
        airc.set_scoped_state(
            ScopeRef::User(airc.peer_id()),
            CONFIG_KEY,
            serde_json::to_string(&config()).unwrap(),
            1,
        )
        .await
        .unwrap();
        let first = prepare(&airc, "codex:session-a", false)
            .await
            .unwrap()
            .unwrap();
        assert!(
            prepare(&airc, "codex:session-a", false)
                .await
                .unwrap()
                .is_some(),
            "failed writer has not acknowledged"
        );
        acknowledge(&airc, first).await.unwrap();
        assert!(prepare(&airc, "codex:session-a", false)
            .await
            .unwrap()
            .is_none());
        assert!(prepare(&airc, "claude:session-b", false)
            .await
            .unwrap()
            .is_some());
        drop(airc);
        let resumed = Airc::open_as(&home, "ResumeFixture").await.unwrap();
        assert!(prepare(&resumed, "codex:session-a", false)
            .await
            .unwrap()
            .is_none());
        assert!(prepare(&resumed, "codex:new-session", true)
            .await
            .unwrap()
            .is_none());
        assert!(prepare(&resumed, "codex:new-session", false)
            .await
            .unwrap()
            .is_some());
    }
}
