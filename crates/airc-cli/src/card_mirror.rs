//! `airc work mirror run`: the card mirror (card 9681e5b5, slice 1b). Projects one room's
//! board onto GitHub issues of one repo, behind the board and never on the claim path.
//!
//! Joel, 2026-09-28: a project's cards, and who did what, visible just by going to GitHub.
//! Scope (Joel): only NEW work mirrors automatically, meaning cards created after the repo
//! opted in (the first run records that moment); existing issues come in one at a time by an
//! explicit import, never a bulk sweep of a backlog full of stale issues.
//!
//! Every decision is the pure `airc_work::issue_mirror` core; this file only resolves names,
//! signs, reads and writes GitHub, and remembers what it pushed:
//! - a card whose wanted issue equals what the mirror last saw for it makes NO GitHub call;
//! - the card's issue is found by its `airc-card:<id>` label before any create, so a create
//!   that timed out is found rather than duplicated;
//! - a field someone else edited on GitHub is a conflict: left alone and reported;
//! - the state file is written after every card, so a crash between a remote write and the
//!   save costs at most one re-read, never a duplicate issue.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use airc_lib::gh::client::{
    GhClient, IssueCreateArgs, IssueEditArgs, IssueListArgs, IssueRecord, IssueWireState,
};
use airc_lib::{Airc, WorkCard};
use airc_work::issue_mirror::{
    self, CardView, IssueField, IssueSpec, IssueState, MirrorAction, Participant, RemoteIssue,
};
use serde::{Deserialize, Serialize};

/// The domain tag the mirror signs a card's metadata under.
const SIGN_CONTEXT: &str = "airc-card-mirror:v1";

/// What the mirror remembers about one card.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Mirrored {
    /// The issue the card projects to.
    number: u64,
    /// The last spec the mirror WROTE to that issue; `None` until its first write.
    pushed: Option<IssueSpec>,
    /// The last spec the mirror PROCESSED for this card (written, unchanged or in conflict):
    /// equal to what the card wants now means nothing to do and no GitHub call.
    seen: IssueSpec,
    /// Fields left alone because someone else edited them on GitHub, if any.
    #[serde(default)]
    conflicted: BTreeSet<IssueField>,
}

/// The mirror's durable state for one (room, repo): when the repo opted in, and each card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MirrorState {
    opted_in_at_ms: u64,
    cards: BTreeMap<String, Mirrored>,
}

fn state_path(home: &Path, repo: &str) -> PathBuf {
    home.join("card-mirror")
        .join(format!("{}.json", repo.replace('/', "__")))
}

fn load_state(path: &Path) -> Result<Option<MirrorState>, Box<dyn std::error::Error>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save_state(path: &Path, state: &MirrorState) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0) // a pre-epoch clock opts in at 0: every existing card would mirror, visibly
}

/// Run the mirror loop for `repo` on `room` (the current room when `None`) until shutdown.
pub async fn run(
    home: &Path,
    room: Option<String>,
    repo: String,
    interval: Duration,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = crate::merger::acquire_named_lock(home, "card-mirror")?;
    let socket = crate::cli::default_socket_path_in(home);
    crate::commands::ensure_daemon_running(home, socket.clone(), Vec::new()).await?;
    let airc = Airc::attach(home, socket).await?;
    let gh = crate::gh_reqwest::production_gh_client();
    let path = state_path(home, &repo);
    let mut state = match load_state(&path)? {
        Some(state) => state,
        None => {
            let fresh = MirrorState {
                opted_in_at_ms: now_ms(),
                cards: BTreeMap::new(),
            };
            save_state(&path, &fresh)?;
            fresh
        }
    };
    eprintln!(
        "airc-card-mirror: started (repo={repo}, room={}, opted_in_at_ms={}, dry_run={dry_run}); cards created before opting in are imported one at a time, never swept",
        room.as_deref().unwrap_or("<current>"),
        state.opted_in_at_ms
    );
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                eprintln!("airc-card-mirror: shutdown signal received, exiting cleanly");
                return Ok(());
            }
            _ = ticker.tick() => {
                if let Err(error) = tick_once(gh.as_ref(), &airc, room.as_deref(), &repo, &path, &mut state, dry_run).await {
                    // a GitHub outage, a rate limit or a daemon blip delays the copy; the board
                    // is untouched and the next tick resumes where this one stopped
                    eprintln!("airc-card-mirror: tick failed: {error}");
                }
            }
        }
    }
}

async fn participant(airc: &Airc, peer: airc_core::PeerId) -> Participant {
    let peer_short: String = peer.to_string().chars().take(8).collect();
    let name = airc
        .peer_alias(peer)
        .await
        .ok()
        .flatten()
        .filter(|n| !n.trim().is_empty());
    Participant { name, peer_short }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn remote_of(record: &IssueRecord) -> RemoteIssue {
    RemoteIssue {
        number: record.number,
        title: record.title.clone(),
        body: record.body.clone(),
        state: if record.state == "CLOSED" {
            IssueState::Closed
        } else {
            IssueState::Open
        },
        labels: record.labels.iter().cloned().collect(),
    }
}

fn wire(state: IssueState) -> IssueWireState {
    match state {
        IssueState::Open => IssueWireState::Open,
        IssueState::Closed => IssueWireState::Closed,
    }
}

/// Whether the mirror handles this card: its repo, and new work (created after opting in),
/// or a card already linked (an explicit import).
fn in_scope(card: &WorkCard, repo: &str, state: &MirrorState) -> bool {
    card.repo.as_str() == repo
        && (card.created_at_ms >= state.opted_in_at_ms
            || state
                .cards
                .contains_key(&card.card_id.as_uuid().to_string()))
}

async fn tick_once(
    gh: &dyn GhClient,
    airc: &Airc,
    room: Option<&str>,
    repo: &str,
    path: &Path,
    state: &mut MirrorState,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let room = match room {
        Some(name) => {
            airc.room_by_name_or_channel(name, "mirror the board of")
                .await?
        }
        None => airc.current_room().await?,
    };
    let board = airc.work_board_in(&room).await?;
    let snapshot = board.snapshot();
    let in_scope_cards: Vec<&WorkCard> = snapshot
        .cards
        .iter()
        .filter(|c| in_scope(c, repo, state))
        .collect();
    for card in in_scope_cards {
        let key = card.card_id.as_uuid().to_string();
        let view = CardView {
            card,
            reviews: board.review_cards_for(card.card_id).collect(),
            created_by: participant(airc, card.created_by).await,
            holder: match card.owner {
                Some(owner) => Some(participant(airc, owner).await),
                None => None,
            },
        };
        let payload = issue_mirror::metadata_payload(card);
        let signature = hex(&airc
            .sign_assertion(SIGN_CONTEXT, payload.as_bytes())
            .signature);
        let desired = issue_mirror::desired_issue(&view, Some(&signature));
        if state.cards.get(&key).is_some_and(|m| m.seen == desired) {
            continue; // nothing changed since the mirror last handled it: no GitHub call
        }
        let found = gh
            .issue_list_by_label(IssueListArgs {
                repo: repo.to_string(),
                label: issue_mirror::card_label(card),
            })
            .await?;
        // the lowest number is the original; any other carrying the label is a duplicate to report
        let remote = found.iter().min_by_key(|r| r.number).map(remote_of);
        if found.len() > 1 {
            eprintln!(
                "airc-card-mirror: card {key} has {} issues carrying its label; using #{}",
                found.len(),
                remote.as_ref().map_or(0, |r| r.number)
            );
        }
        let known = state.cards.get(&key).cloned();
        let action = issue_mirror::next_action(
            remote.as_ref(),
            known.as_ref().and_then(|m| m.pushed.as_ref()),
            &desired,
        );
        let seen = Seen {
            key: &key,
            desired: &desired,
            known,
            remote_number: remote.as_ref().map(|r| r.number),
        };
        let record = apply(gh, repo, action, seen, dry_run).await?;
        if let Some(record) = record {
            state.cards.insert(key, record);
            save_state(path, state)?;
        }
    }
    Ok(())
}

/// What the mirror saw for one card on this tick.
struct Seen<'a> {
    key: &'a str,
    desired: &'a IssueSpec,
    known: Option<Mirrored>,
    remote_number: Option<u64>,
}

/// Apply one action; returns what to remember for the card (`None` in a dry run).
async fn apply(
    gh: &dyn GhClient,
    repo: &str,
    action: MirrorAction,
    seen: Seen<'_>,
    dry_run: bool,
) -> Result<Option<Mirrored>, Box<dyn std::error::Error>> {
    let Seen {
        key,
        desired,
        known,
        remote_number,
    } = seen;
    if dry_run {
        eprintln!("airc-card-mirror: [dry-run] card {key}: {action:?}");
        return Ok(None);
    }
    Ok(Some(match action {
        MirrorAction::Create(spec) => {
            let number = gh
                .issue_create(IssueCreateArgs {
                    repo: repo.to_string(),
                    title: spec.title.clone(),
                    body: spec.body.clone(),
                    labels: spec.labels.iter().cloned().collect(),
                })
                .await?;
            if spec.state == IssueState::Closed {
                gh.issue_edit(IssueEditArgs {
                    repo: repo.to_string(),
                    number,
                    title: None,
                    body: None,
                    state: Some(IssueWireState::Closed),
                    labels: None,
                })
                .await?;
            }
            eprintln!("airc-card-mirror: card {key} -> created #{number}");
            Mirrored {
                number,
                pushed: Some(spec),
                seen: desired.clone(),
                conflicted: BTreeSet::new(),
            }
        }
        MirrorAction::Update {
            number,
            fields,
            spec,
            labels,
        } => {
            gh.issue_edit(IssueEditArgs {
                repo: repo.to_string(),
                number,
                title: fields
                    .contains(&IssueField::Title)
                    .then(|| spec.title.clone()),
                body: fields
                    .contains(&IssueField::Body)
                    .then(|| spec.body.clone()),
                state: fields
                    .contains(&IssueField::State)
                    .then(|| wire(spec.state)),
                labels: Some(labels.into_iter().collect()),
            })
            .await?;
            eprintln!("airc-card-mirror: card {key} -> updated #{number} ({fields:?})");
            Mirrored {
                number,
                pushed: Some(spec),
                seen: desired.clone(),
                conflicted: BTreeSet::new(),
            }
        }
        MirrorAction::Unchanged => {
            // Unchanged is only returned for an issue that exists, so its number is known
            let Some(number) = remote_number.or(known.as_ref().map(|m| m.number)) else {
                return Err(format!("card {key}: an unchanged issue with no number").into());
            };
            Mirrored {
                number,
                pushed: Some(desired.clone()),
                seen: desired.clone(),
                conflicted: BTreeSet::new(),
            }
        }
        MirrorAction::Conflict { number, fields } => {
            eprintln!(
                "airc-card-mirror: card {key} #{number}: {fields:?} changed on GitHub since the mirror wrote it; left as they are (a conflict to resolve by a typed edit)"
            );
            Mirrored {
                number,
                pushed: known.and_then(|m| m.pushed),
                seen: desired.clone(),
                conflicted: fields,
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches (Joel: only new work mirrors; the backlog of stale issues is imported
    // one at a time): a card created before the repo opted in being swept into GitHub, or an
    // explicitly imported (already linked) card being skipped for its age.
    #[test]
    fn only_new_work_or_an_imported_card_is_in_scope() {
        let mut card: WorkCard = serde_json::from_value(serde_json::json!({
            "card_id": uuid::Uuid::new_v4(),
            "repo": "CambrianTech/career-wrangler",
            "title": "t", "body": null, "priority": "p2", "lane_id": null, "state": "open",
            "owner": null, "claim_id": null, "claim_expires_at_ms": null,
            "last_heartbeat_at_ms": null, "pull_request": null,
            "created_by": uuid::Uuid::new_v4(), "created_at_ms": 100, "updated_at_ms": 100
        }))
        .expect("test: a card");
        let mut state = MirrorState {
            opted_in_at_ms: 500,
            cards: BTreeMap::new(),
        };
        assert!(
            !in_scope(&card, "CambrianTech/career-wrangler", &state),
            "older than the opt-in: not swept"
        );
        card.created_at_ms = 600;
        assert!(
            in_scope(&card, "CambrianTech/career-wrangler", &state),
            "new work mirrors"
        );
        assert!(
            !in_scope(&card, "CambrianTech/continuum", &state),
            "another repo is not this mirror's"
        );
        card.created_at_ms = 100;
        let spec = IssueSpec {
            title: "t".into(),
            body: "b".into(),
            state: IssueState::Open,
            labels: BTreeSet::new(),
        };
        state.cards.insert(
            card.card_id.as_uuid().to_string(),
            Mirrored {
                number: 7,
                pushed: None,
                seen: spec,
                conflicted: BTreeSet::new(),
            },
        );
        assert!(
            in_scope(&card, "CambrianTech/career-wrangler", &state),
            "an imported card mirrors whatever its age"
        );
    }
}
