use std::error::Error;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::collaboration_cli::CollaborationScopeArgs;

pub async fn run_status(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    let count = peer_record_count(home).await;

    if count == 0 {
        println!("  collaboration: waiting for peers (0 peer records)");
        println!(
            "    First agent in a room is expected to be alone until another agent joins this account mesh."
        );
    } else {
        println!("  collaboration: ok ({count} peer record(s))");
    }
    Ok(())
}

pub async fn run_doctor(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    let count = peer_record_count(home).await;
    if count > 0 {
        println!("  [ok] collaboration mesh has {count} peer record(s)");
        return Ok(());
    }
    println!("  [info] collaboration mesh has 0 peer records — waiting for first peer");
    println!("         Ask the peer to run `airc join`; first-user startup is OK.");
    Ok(())
}

pub async fn run_send_warning(
    default_home: &Path,
    args: CollaborationScopeArgs,
) -> Result<(), Box<dyn Error>> {
    let home = args.home.as_deref().unwrap_or(default_home);
    if peer_record_count(home).await == 0 {
        eprintln!(
            "  WARN: collaboration has no peer records. Run `airc peers` and verify others joined this account mesh."
        );
    }
    Ok(())
}

pub fn command_exit_code(_error: &(dyn Error + 'static)) -> Option<u8> {
    None
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

#[cfg(test)]
mod tests {
    use super::*;

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
