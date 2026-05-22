use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::Path;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;

use crate::collaboration_cli::CollaborationScopeArgs;

const RECENT_REMOTE_WINDOW_SEC: i64 = 600;

#[derive(Clone, Debug, Eq, PartialEq)]
struct MessageActivity {
    name: String,
    ts: i64,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct CollaborationEvidence {
    recent_speakers: BTreeMap<String, i64>,
    recent_activity: Option<MessageActivity>,
    any_activity: Option<MessageActivity>,
}

pub async fn run_status(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    let count = peer_record_count(home).await;
    let evidence = collaboration_evidence(home, &args.my_name, &args.client_id);
    let any_recent = evidence
        .recent_activity
        .as_ref()
        .or(evidence.any_activity.as_ref());
    let now = Utc::now().timestamp();
    let remote_desc = match any_recent {
        Some(activity) => format!(
            "last remote message {}s ago from {}",
            (now - activity.ts).max(0),
            activity.name
        ),
        None => "no remote messages recorded".to_string(),
    };

    if count == 0 {
        if !evidence.recent_speakers.is_empty() {
            let label = if evidence.recent_speakers.len() == 1 {
                "broadcast peer"
            } else {
                "broadcast peers"
            };
            println!(
                "  collaboration: ok ({} {label}; 0 direct peer records; {remote_desc})",
                evidence.recent_speakers.len()
            );
            println!("    Presence is derived from recent signed room traffic.");
        } else if any_recent.is_none() {
            println!("  collaboration: waiting for peers (0 peer records; {remote_desc})");
            println!(
                "    First agent in a room is expected to be alone until another agent joins this gist."
            );
        } else {
            println!("  collaboration: SOLO (0 peer records; {remote_desc})");
            println!(
                "    Sends may only land in this local/self-hosted gist until another agent joins this exact mesh."
            );
        }
    } else {
        println!("  collaboration: ok ({count} peer record(s); {remote_desc})");
    }
    Ok(())
}

pub async fn run_doctor(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    let count = peer_record_count(home).await;
    let evidence = collaboration_evidence(home, &args.my_name, &args.client_id);
    let now = Utc::now().timestamp();
    if count > 0 {
        println!("  [ok] collaboration mesh has {count} peer record(s)");
        return Ok(());
    }
    if let Some(recent) = evidence.recent_activity.as_ref() {
        if !evidence.recent_speakers.is_empty() {
            let label = if evidence.recent_speakers.len() == 1 {
                "broadcast peer"
            } else {
                "broadcast peers"
            };
            println!(
                "  [ok] collaboration mesh has {} recent {label} from signed room traffic (0 direct peer records)",
                evidence.recent_speakers.len()
            );
            println!(
                "       last remote message {}s ago from {}",
                (now - recent.ts).max(0),
                recent.name
            );
            return Ok(());
        }
        println!(
            "  [WARN] collaboration mesh has 0 peer records, but remote traffic arrived {}s ago from {}",
            (now - recent.ts).max(0),
            recent.name
        );
        println!(
            "         Peer metadata is degraded (DMs/whois may fail), but this is NOT a solo island."
        );
        return Err(command_exit(1));
    }
    let Some(any_recent) = evidence.any_activity.as_ref() else {
        println!(
            "  [info] collaboration mesh has 0 peer records and no remote history — waiting for first peer"
        );
        println!("         Share the invite or ask another agent to join this room; first-user startup is OK.");
        return Ok(());
    };
    println!(
        "  [BLOCKED] collaboration mesh has 0 peer records — last remote traffic was {}s ago from {}; this may be a solo island",
        (now - any_recent.ts).max(0),
        any_recent.name
    );
    println!(
        "         Check: airc peers; ask peers to run 'airc update --channel canary && airc join <current invite>'"
    );
    Err(command_exit(2))
}

pub async fn run_send_warning(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    if peer_record_count(home).await == 0
        && recent_remote_speakers(home, &args.my_name, &args.client_id).is_empty()
    {
        eprintln!(
            "  WARN: collaboration has no direct peer records or recent broadcast peers. Run 'airc peers' and verify others joined this gist."
        );
    }
    Ok(())
}

pub fn run_observed_whois(
    default_home: &Path,
    args: CollaborationScopeArgs,
    peer_name: &str,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    let speakers = recent_remote_speakers(home, &args.my_name, &args.client_id);
    let Some(ts) = speakers.get(peer_name) else {
        return Err(command_exit(1));
    };
    println!("  name:      {peer_name}");
    println!("  pronouns:  (unknown)");
    println!("  role:      observed room participant");
    println!("  bio:       seen in recent signed room traffic");
    println!("  status:    (unknown)");
    println!("  integrations: (none)");
    println!(
        "  presence:  observed from room traffic, last seen {}",
        fmt_age(*ts)
    );
    Ok(())
}

pub fn command_exit_code(error: &(dyn Error + 'static)) -> Option<u8> {
    error.downcast_ref::<CommandExit>().map(|error| error.0)
}

async fn peer_record_count(home: &Path) -> usize {
    legacy_peer_record_count(home) + rust_peer_record_count(home).await
}

/// Count peers from the legacy `<home>/peers/<peer>.json` per-peer
/// directory (bash-wrapper-era storage).
fn legacy_peer_record_count(home: &Path) -> usize {
    let peers_dir = home.join("peers");
    let Ok(entries) = fs::read_dir(peers_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter(|entry| {
            fs::read_to_string(entry.path())
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .is_some()
        })
        .count()
}

/// Count peers in the Rust substrate's peer registry. Two locations
/// matter:
///   1. `<home>/events.sqlite` — per-scope peer trust rows written
///      when a peer is enrolled via this scope.
///   2. `$HOME/.airc/events.sqlite` — the machine-account peer trust store
///      shared by all scopes on this user's machine. `Airc::open`
///      adds every loaded identity to BOTH, so a scope running from
///      a project subdir still publishes its peer record into the
///      machine-wide registry.
///
/// Without counting the second path, `airc status` says SOLO even
/// when another scope on the same machine has enrolled, which made
/// the substrate look broken when it was actually working.
async fn rust_peer_record_count(home: &Path) -> usize {
    use std::collections::HashSet;

    let mut seen: HashSet<airc_core::PeerId> = HashSet::new();
    for path in rust_peer_registry_paths(home) {
        if let Ok(peers) = airc_daemon::peers_store::load(&path).await {
            for peer in peers {
                seen.insert(peer.peer_id);
            }
        }
    }
    seen.len()
}

fn rust_peer_registry_paths(home: &Path) -> Vec<std::path::PathBuf> {
    let mut paths = vec![home.to_path_buf()];
    if let Some(machine) = machine_account_home_for(home) {
        if machine != home {
            paths.push(machine);
        }
    }
    paths
}

/// Resolve the machine-account home (`$HOME/.airc`) only if the
/// inspected scope home is itself under `$HOME`. This mirrors the
/// `airc-lib` logic: a scope rooted under the user's home dir shares
/// the machine-account wire + peer registry; a scope in an arbitrary
/// path (CI tempdirs, hermetic tests) does NOT pull in real-world
/// state. Keeps `peer_record_count` hermetic in tests while still
/// surfacing same-machine peers when the user actually runs `airc`.
fn machine_account_home_for(scope_home: &Path) -> Option<std::path::PathBuf> {
    let user_home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)?;
    let canon_user_home = user_home.canonicalize().unwrap_or(user_home);
    let canon_scope = scope_home
        .canonicalize()
        .unwrap_or_else(|_| scope_home.to_path_buf());
    if canon_scope.starts_with(&canon_user_home) {
        Some(canon_user_home.join(".airc"))
    } else {
        None
    }
}

fn collaboration_evidence(home: &Path, my_name: &str, my_client_id: &str) -> CollaborationEvidence {
    let mut evidence = CollaborationEvidence::default();
    let now = Utc::now().timestamp();
    for activity in remote_messages(home, my_name, None, my_client_id) {
        if evidence
            .any_activity
            .as_ref()
            .is_none_or(|current| activity.ts > current.ts)
        {
            evidence.any_activity = Some(activity.clone());
        }
        if now - activity.ts >= RECENT_REMOTE_WINDOW_SEC {
            continue;
        }
        evidence
            .recent_speakers
            .entry(activity.name.clone())
            .and_modify(|ts| *ts = (*ts).max(activity.ts))
            .or_insert(activity.ts);
        if evidence
            .recent_activity
            .as_ref()
            .is_none_or(|current| activity.ts > current.ts)
        {
            evidence.recent_activity = Some(activity);
        }
    }
    evidence
}

fn recent_remote_speakers(home: &Path, my_name: &str, my_client_id: &str) -> BTreeMap<String, i64> {
    let mut speakers = BTreeMap::new();
    for activity in remote_messages(home, my_name, Some(RECENT_REMOTE_WINDOW_SEC), my_client_id) {
        speakers
            .entry(activity.name)
            .and_modify(|ts: &mut i64| *ts = (*ts).max(activity.ts))
            .or_insert(activity.ts);
    }
    speakers
}

fn remote_messages(
    home: &Path,
    my_name: &str,
    window_sec: Option<i64>,
    my_client_id: &str,
) -> Vec<MessageActivity> {
    let now = Utc::now().timestamp();
    fs::read_to_string(home.join("messages.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| !is_self_message(message, my_name, my_client_id))
        .filter_map(|message| {
            let ts = epoch(message.get("ts")?.as_str()?)?;
            if window_sec.is_some_and(|window| now - ts >= window) {
                return None;
            }
            let sender = message.get("from")?.as_str()?;
            let client_id = message
                .get("client_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(MessageActivity {
                name: display_name(sender, client_id, my_name),
                ts,
            })
        })
        .collect()
}

fn is_self_message(message: &Value, my_name: &str, my_client_id: &str) -> bool {
    let Some(sender) = message.get("from").and_then(Value::as_str) else {
        return true;
    };
    if sender == "airc" {
        return true;
    }
    let msg_client_id = message
        .get("client_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !my_client_id.is_empty() && !msg_client_id.is_empty() {
        return msg_client_id == my_client_id;
    }
    sender == my_name && msg_client_id.is_empty()
}

fn display_name(sender: &str, client_id: &str, my_name: &str) -> String {
    if !client_id.is_empty() && sender == my_name {
        format!("{sender} [{client_id}]")
    } else {
        sender.to_string()
    }
}

fn epoch(ts: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .map(|value| value.timestamp())
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(ts.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|value| Utc.from_utc_datetime(&value).timestamp())
        })
}

fn fmt_age(ts: i64) -> String {
    let age = (Utc::now().timestamp() - ts).max(0);
    if age < 60 {
        format!("{age}s ago")
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86400)
    }
}

#[derive(Debug)]
struct CommandExit(u8);

impl std::fmt::Display for CommandExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "command exited with {}", self.0)
    }
}

impl Error for CommandExit {}

fn command_exit(code: u8) -> Box<dyn Error> {
    Box::new(CommandExit(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_speakers_excludes_self_by_client_id() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("messages.jsonl"),
            format!(
                "{}\n{}\n",
                r#"{"from":"me","client_id":"mine","ts":"2026-05-19T12:00:00Z"}"#,
                r#"{"from":"me","client_id":"peer-tab","ts":"2026-05-19T12:00:01Z"}"#,
            ),
        )
        .unwrap();

        let messages = remote_messages(dir.path(), "me", None, "mine");

        assert_eq!(
            messages,
            vec![MessageActivity {
                name: "me [peer-tab]".to_string(),
                ts: 1_779_192_001,
            }]
        );
    }

    #[tokio::test]
    async fn peer_record_count_requires_valid_json_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("peers")).unwrap();
        fs::write(dir.path().join("peers").join("alice.json"), "{}").unwrap();
        fs::write(dir.path().join("peers").join("broken.json"), "{").unwrap();
        fs::write(dir.path().join("peers").join("note.txt"), "{}").unwrap();

        assert_eq!(peer_record_count(dir.path()).await, 1);
    }
}
