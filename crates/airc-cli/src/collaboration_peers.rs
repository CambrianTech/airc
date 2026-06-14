use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::collaboration_cli::CollaborationScopeArgs;

#[derive(Debug, Clone, Eq, PartialEq)]
struct PeerRecord {
    name: String,
    host: String,
    paired: String,
    stem: String,
}

pub async fn run_peers(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    // Seam #2 (solidification doc): the airc_trust store is the CANONICAL
    // peer truth (peer_id + tier + endpoints). `collaboration peers` is
    // now a VIEW over it via the shared `peer list` renderer — NOT the
    // legacy file-based name/host/paired records under
    // `<home>/peers/*.json`. Those files persist only as handshake
    // pairing metadata; fully retiring them + migrating the handshake
    // flow onto the trust store (and dropping `prune-peers`) is the
    // remaining #2 work, tracked. One peer truth, one place.
    //
    // Output-shape contract change (Mac review of #1193): the rendered
    // lines went from `name -> host [#room]` to
    // `peer_id  pubkey  tier=…`. A grep across docs/skills/install
    // scripts confirmed nothing parses the old shape, but humans with it
    // memorised get a one-line stderr heads-up. stderr (not stdout) so a
    // pipe consuming the peer list stays byte-for-byte unaffected.
    eprintln!(
        "note: `collaboration peers` now renders the canonical trust store \
         (peer_id/pubkey/tier); the legacy name/host file view is retired. \
         Prefer `airc peer list`."
    );
    crate::commands::run_peer_list(home, false).await
}

/// Prune duplicate legacy `<home>/peers/*.json` records (newest wins
/// per host).
///
/// Seam #2 ASYMMETRY (Mac review of #1193): this still reads/writes the
/// **legacy file** surface, while [`run_peers`] above reads the trust
/// store. That is a deliberate temporary fanout — three peer surfaces
/// are briefly in flight (trust store + `handshake_commands.rs` file
/// writer + this file reader). Retiring the file writer and dropping
/// this command is the remaining seam #2 slice; do NOT build new
/// behaviour on this path.
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

fn remove_peer_file(peers_dir: &Path, stem: &str, extension: &str) {
    let mut path = PathBuf::from(peers_dir);
    path.push(format!("{stem}.{extension}"));
    if path.is_file() {
        let _ = fs::remove_file(path);
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
}
