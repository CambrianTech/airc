use std::error::Error;
use std::fs;
use std::path::Path;

use serde_json::{json, Value};

const IDENTITY_FIELDS: &[&str] = &["pronouns", "role", "bio", "status"];

pub fn run_pretty(name: &str, identity_json: &str, host: &str) -> Result<(), Box<dyn Error>> {
    let identity: Value =
        serde_json::from_str(identity_json).unwrap_or_else(|_| Value::Object(Default::default()));

    println!("  name:      {name}");
    for field in IDENTITY_FIELDS {
        let label = format!("{field}:");
        let value = identity
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("(unset)");
        println!("  {label:<11} {value}");
    }

    let integrations = identity.get("integrations").and_then(Value::as_object);
    if let Some(integrations) = integrations.filter(|items| !items.is_empty()) {
        println!("  integrations:");
        for (key, value) in integrations {
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            println!("    {key}: {text}");
        }
    } else {
        println!("  integrations: (none)");
    }

    if !host.is_empty() {
        println!("  host:      {host}");
    }
    Ok(())
}

pub fn run_write_peer_record(
    peers_dir: &Path,
    peer_name: &str,
    host: &str,
    airc_home: &str,
    x25519_pub: &str,
    paired: &str,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(peers_dir)?;
    remove_stale_host_records(peers_dir, peer_name, host)?;

    let mut record = json!({
        "name": peer_name,
        "host": host,
        "airc_home": airc_home,
        "paired": paired,
    });
    if !x25519_pub.is_empty() {
        record["x25519_pub"] = Value::String(x25519_pub.to_string());
    }

    let path = peers_dir.join(format!("{peer_name}.json"));
    fs::write(path, serde_json::to_string_pretty(&record)?)?;
    Ok(())
}

pub fn run_peer_ssh_pub(peers_dir: &Path, peer_name: &str) -> Result<(), Box<dyn Error>> {
    if let Some(ssh_pub) = peer_ssh_pub(peers_dir, peer_name) {
        println!("{ssh_pub}");
    }
    Ok(())
}

fn peer_ssh_pub(peers_dir: &Path, peer_name: &str) -> Option<String> {
    let path = peers_dir.join(format!("{peer_name}.json"));
    let value = fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    value
        .get("ssh_pub")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn remove_stale_host_records(
    peers_dir: &Path,
    peer_name: &str,
    host: &str,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(peers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if path.file_stem().and_then(|value| value.to_str()) == Some(peer_name) {
            continue;
        }
        let stale = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| {
                value
                    .get("host")
                    .and_then(Value::as_str)
                    .map(|stored_host| stored_host == host)
            })
            .unwrap_or(false);
        if stale {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(path.with_extension("pub"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_identity_is_treated_as_empty() {
        assert!(run_pretty("alice", "not-json", "").is_ok());
    }

    #[test]
    fn write_peer_record_removes_stale_records_for_same_host() {
        let dir = tempfile::tempdir().unwrap();
        let peers = dir.path();
        fs::write(
            peers.join("old.json"),
            r#"{"name":"old","host":"alice@example","airc_home":"/old"}"#,
        )
        .unwrap();
        fs::write(peers.join("old.pub"), "legacy").unwrap();

        run_write_peer_record(peers, "new", "alice@example", "/airc", "xpub", "now").unwrap();

        assert!(!peers.join("old.json").exists());
        assert!(!peers.join("old.pub").exists());
        let written: Value =
            serde_json::from_str(&fs::read_to_string(peers.join("new.json")).unwrap()).unwrap();
        assert_eq!(written["host"], "alice@example");
        assert_eq!(written["x25519_pub"], "xpub");
    }

    #[test]
    fn peer_ssh_pub_reads_peer_record() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("alice.json"),
            r#"{"ssh_pub":"ssh-ed25519 AAAAC3Nz alice"}"#,
        )
        .unwrap();

        assert_eq!(
            peer_ssh_pub(dir.path(), "alice").as_deref(),
            Some("ssh-ed25519 AAAAC3Nz alice")
        );
        assert_eq!(peer_ssh_pub(dir.path(), "missing"), None);
    }
}
