use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;

use crate::collaboration_cli::CollaborationScopeArgs;

const HEARTBEAT_KIND: &str = "heartbeat";
const STALE_HEARTBEAT_SEC: i64 = 120;
const RECENT_BROADCAST_WINDOW_SEC: i64 = 600;

#[derive(Debug, Clone, Eq, PartialEq)]
struct PeerRecord {
    name: String,
    host: String,
    paired: String,
    stem: String,
}

#[derive(Debug, Default)]
struct MessagePresence {
    last_message: BTreeMap<String, i64>,
    last_heartbeat: BTreeMap<String, i64>,
}

pub fn run_peers(default_home: &Path, args: CollaborationScopeArgs) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    let peers = peer_records(home);
    let presence = message_presence(home, &args.my_name, &args.client_id);
    if peers.is_empty() {
        print_broadcast_or_empty(&presence, &args.my_name);
        return Ok(());
    }
    print_peer_records(home, &peers, &presence);
    print_broadcast_only(&presence, &rendered_names(&peers), &args.my_name);
    Ok(())
}

pub fn run_prune_peers(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    let peers = peer_records(home);
    let mut by_host: BTreeMap<String, Vec<PeerRecord>> = BTreeMap::new();
    for peer in peers {
        if !peer.host.is_empty() {
            by_host.entry(peer.host.clone()).or_default().push(peer);
        }
    }

    let peers_dir = home.join("peers");
    let mut removed = Vec::new();
    for (host, mut records) in by_host {
        if records.len() < 2 {
            continue;
        }
        records.sort_by(|left, right| {
            right
                .paired
                .cmp(&left.paired)
                .then_with(|| right.stem.cmp(&left.stem))
        });
        for record in records.into_iter().skip(1) {
            remove_peer_file(&peers_dir, &record.stem, "json");
            remove_peer_file(&peers_dir, &record.stem, "pub");
            removed.push((record.name, host.clone()));
        }
    }

    if removed.is_empty() {
        println!("  No stale records to prune.");
    } else {
        for (name, host) in removed {
            println!("  pruned: {name} -> {host}");
        }
    }
    Ok(())
}

fn peer_records(home: &Path) -> Vec<PeerRecord> {
    let peers_dir = home.join("peers");
    let Ok(entries) = fs::read_dir(peers_dir) else {
        return Vec::new();
    };
    let mut records = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| peer_record_from_path(&entry.path()))
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.host.cmp(&right.host))
    });
    records
}

fn peer_record_from_path(path: &Path) -> Option<PeerRecord> {
    let value = serde_json::from_str::<Value>(&fs::read_to_string(path).ok()?).ok()?;
    let stem = path.file_stem()?.to_string_lossy().to_string();
    Some(PeerRecord {
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&stem)
            .to_string(),
        host: value
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        paired: value
            .get("paired")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        stem,
    })
}

fn message_presence(home: &Path, my_name: &str, my_client_id: &str) -> MessagePresence {
    let mut presence = MessagePresence::default();
    let raw = fs::read_to_string(home.join("messages.jsonl")).unwrap_or_default();
    for message in raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let Some(who) = message_sender(&message, my_name, my_client_id) else {
            continue;
        };
        let Some(ts) = message.get("ts").and_then(Value::as_str).and_then(epoch) else {
            continue;
        };
        let target = if message.get("kind").and_then(Value::as_str) == Some(HEARTBEAT_KIND) {
            &mut presence.last_heartbeat
        } else {
            &mut presence.last_message
        };
        target
            .entry(who)
            .and_modify(|current| *current = (*current).max(ts))
            .or_insert(ts);
    }
    presence
}

fn message_sender(message: &Value, my_name: &str, my_client_id: &str) -> Option<String> {
    let sender = message.get("from").and_then(Value::as_str)?;
    let client_id = message
        .get("client_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !my_client_id.is_empty() && client_id == my_client_id {
        return None;
    }
    if sender == my_name {
        if client_id.is_empty() {
            return None;
        }
        return Some(format!("{sender} [{client_id}]"));
    }
    Some(sender.to_string())
}

fn print_peer_records(home: &Path, peers: &[PeerRecord], presence: &MessagePresence) {
    let room = fs::read_to_string(home.join("room_name"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "(?)".to_string());
    let last_seen = last_seen(presence);
    let now = Utc::now().timestamp();
    let mut seen_keys = BTreeSet::new();
    for peer in peers {
        if !seen_keys.insert((peer.name.clone(), peer.host.clone())) {
            continue;
        }
        let last_ts = last_seen.get(&peer.name).copied();
        let hb_ts = presence.last_heartbeat.get(&peer.name).copied();
        println!(
            "  {} -> {}   [#{}]   last seen {}{}",
            peer.name,
            peer.host,
            room,
            last_ts.map(fmt_age).unwrap_or_else(|| "never".to_string()),
            silent_flag(now, last_ts, hb_ts)
        );
    }
}

fn print_broadcast_or_empty(presence: &MessagePresence, my_name: &str) {
    let rendered = BTreeSet::new();
    let rows = broadcast_rows(presence, &rendered, my_name);
    if rows.is_empty() {
        println!("  No peers yet.");
        return;
    }
    println!("  Recent broadcast peers:");
    for (who, ts) in rows {
        println!(
            "  {who} -> broadcast room   [(from signed messages.jsonl)]   last seen {}",
            fmt_age(ts)
        );
    }
}

fn print_broadcast_only(presence: &MessagePresence, rendered: &BTreeSet<String>, my_name: &str) {
    for (who, ts) in broadcast_rows(presence, rendered, my_name) {
        println!(
            "  {who} -> broadcast room   [(from signed messages.jsonl)]   last seen {}",
            fmt_age(ts)
        );
    }
}

fn broadcast_rows(
    presence: &MessagePresence,
    rendered: &BTreeSet<String>,
    my_name: &str,
) -> Vec<(String, i64)> {
    let now = Utc::now().timestamp();
    let mut rows = last_seen(presence)
        .into_iter()
        .filter(|(who, ts)| {
            who != my_name
                && who != "airc"
                && !rendered.contains(who)
                && now - *ts < RECENT_BROADCAST_WINDOW_SEC
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    rows
}

fn rendered_names(peers: &[PeerRecord]) -> BTreeSet<String> {
    peers.iter().map(|peer| peer.name.clone()).collect()
}

fn last_seen(presence: &MessagePresence) -> BTreeMap<String, i64> {
    let mut seen = presence.last_message.clone();
    for (who, ts) in &presence.last_heartbeat {
        seen.entry(who.clone())
            .and_modify(|current| *current = (*current).max(*ts))
            .or_insert(*ts);
    }
    seen
}

fn silent_flag(now: i64, last_ts: Option<i64>, hb_ts: Option<i64>) -> &'static str {
    let Some(last_ts) = last_ts else {
        return " (no recorded activity)";
    };
    let hb_age = hb_ts.map(|ts| now - ts);
    if now - last_ts > 3600 {
        match hb_age {
            None => " (silent)",
            Some(age) if age <= STALE_HEARTBEAT_SEC => " (silent, heartbeat OK)",
            Some(_) => " (PROCESS DOWN)",
        }
    } else if hb_age.is_some_and(|age| age > STALE_HEARTBEAT_SEC) {
        " (heartbeat stale)"
    } else {
        ""
    }
}

fn remove_peer_file(peers_dir: &Path, stem: &str, extension: &str) {
    let mut path = PathBuf::from(peers_dir);
    path.push(format!("{stem}.{extension}"));
    if path.is_file() {
        let _ = fs::remove_file(path);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_newest_peer_per_host() {
        let dir = tempfile::tempdir().unwrap();
        let peers = dir.path().join("peers");
        fs::create_dir(&peers).unwrap();
        fs::write(
            peers.join("old.json"),
            r#"{"name":"old","host":"user@host","paired":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        fs::write(peers.join("old.pub"), "ssh-rsa old").unwrap();
        fs::write(
            peers.join("new.json"),
            r#"{"name":"new","host":"user@host","paired":"2026-01-02T00:00:00Z"}"#,
        )
        .unwrap();

        run_prune_peers(
            dir.path(),
            CollaborationScopeArgs {
                home: None,
                my_name: String::new(),
                client_id: String::new(),
            },
        )
        .unwrap();

        assert!(!peers.join("old.json").exists());
        assert!(!peers.join("old.pub").exists());
        assert!(peers.join("new.json").exists());
    }

    #[test]
    fn broadcast_rows_exclude_rendered_self_and_airc() {
        let mut presence = MessagePresence::default();
        let now = Utc::now().timestamp();
        presence.last_message.insert("alice".to_string(), now);
        presence.last_message.insert("me".to_string(), now);
        presence.last_message.insert("airc".to_string(), now);
        presence.last_message.insert("bob".to_string(), now);

        let rendered = BTreeSet::from(["bob".to_string()]);
        let rows = broadcast_rows(&presence, &rendered, "me");

        assert_eq!(rows, vec![("alice".to_string(), now)]);
    }

    #[test]
    fn message_presence_keeps_same_name_different_client() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("messages.jsonl"),
            r#"{"from":"me","client_id":"mine","ts":"2026-05-19T12:00:00Z"}"#.to_string()
                + "\n"
                + r#"{"from":"me","client_id":"peer-tab","ts":"2026-05-19T12:00:01Z"}"#,
        )
        .unwrap();

        let presence = message_presence(dir.path(), "me", "mine");

        assert!(!presence.last_message.contains_key("me"));
        assert!(presence.last_message.contains_key("me [peer-tab]"));
    }
}
