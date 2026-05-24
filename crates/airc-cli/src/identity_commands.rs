use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use airc_core::identity::Identity;
use airc_identity::LocalIdentity;
use airc_store::{EventStore, SqliteEventStore};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

const IDENTITY_FIELDS: &[&str] = &["pronouns", "role", "bio", "status"];
const PRONOUNS_MAX: usize = 64;
const ROLE_MAX: usize = 128;
const BIO_MAX: usize = 512;
const STATUS_MAX: usize = 256;

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

pub fn run_session_file(write_dir: &Path, transport_name: &str) -> Result<(), Box<dyn Error>> {
    let session_file = session_file(write_dir, transport_name)?;
    println!("{}", session_file.display());
    Ok(())
}

pub fn run_default_work_name(
    transport_name: &str,
    session_file: &Path,
) -> Result<(), Box<dyn Error>> {
    println!("{}", default_work_name(transport_name, session_file));
    Ok(())
}

pub fn run_read_work_name(session_file: &Path) -> Result<(), Box<dyn Error>> {
    let Some(name) = read_work_name(session_file) else {
        return Err("no saved work identity".into());
    };
    println!("{name}");
    Ok(())
}

pub fn run_write_work_session(
    session_file: &Path,
    name: &str,
    transport_name: &str,
) -> Result<(), Box<dyn Error>> {
    write_work_session(session_file, name, transport_name)
}

pub async fn run_show(home: &Path) -> Result<(), Box<dyn Error>> {
    let identity = load_identity_card(home).await?;
    print_identity(&identity);
    Ok(())
}

pub async fn run_set(
    home: &Path,
    pronouns: Option<String>,
    role: Option<String>,
    bio: Option<String>,
    status: Option<String>,
) -> Result<(), Box<dyn Error>> {
    if pronouns.is_none() && role.is_none() && bio.is_none() && status.is_none() {
        return Err("Pass at least one of --pronouns / --role / --bio / --status".into());
    }
    validate_len("pronouns", pronouns.as_deref(), PRONOUNS_MAX)?;
    validate_len("role", role.as_deref(), ROLE_MAX)?;
    validate_len("bio", bio.as_deref(), BIO_MAX)?;
    validate_len("status", status.as_deref(), STATUS_MAX)?;

    let mut identity = load_identity_card(home).await?;
    set_optional_string(&mut identity.pronouns, pronouns);
    set_optional_string(&mut identity.role, role);
    set_optional_string(&mut identity.bio, bio);
    set_optional_string(&mut identity.status, status);
    save_identity_card(home, identity).await?;
    println!("  identity updated.");
    Ok(())
}

pub async fn run_link(home: &Path, platform: &str, handle: &str) -> Result<(), Box<dyn Error>> {
    let mut identity = load_identity_card(home).await?;
    let previous = identity.integrations.get(platform).cloned();
    if handle.trim().is_empty() {
        identity.integrations.remove(platform);
    } else {
        identity
            .integrations
            .insert(platform.to_string(), handle.trim().to_string());
    }
    save_identity_card(home, identity).await?;

    match (handle.trim().is_empty(), previous) {
        (false, Some(prev)) if prev != handle.trim() => {
            println!("  linked: {platform} -> {} (was: {prev})", handle.trim())
        }
        (false, _) => println!("  linked: {platform} -> {}", handle.trim()),
        (true, Some(prev)) => {
            println!("  unlinked: {platform} (was: {prev}; pass a handle to (re)link)")
        }
        (true, None) => println!(
            "  no {platform} integration to unlink. Pass a handle to link: airc identity link {platform} <handle>"
        ),
    }
    Ok(())
}

pub async fn run_nudge_needed(home: &Path) -> Result<(), Box<dyn Error>> {
    let identity = load_identity_card(home).await?;
    if !identity.is_complete() {
        return Err(NudgeNeeded.into());
    }
    Ok(())
}

pub async fn run_import_continuum(home: &Path, blob: &str) -> Result<(), Box<dyn Error>> {
    let source: Value = serde_json::from_str(blob).unwrap_or(Value::Null);
    let mut identity = load_identity_card(home).await?;
    set_from_source(&mut identity.pronouns, &source, "pronouns");
    set_from_source(&mut identity.role, &source, "role");
    set_from_source(&mut identity.bio, &source, "bio");
    let name = source
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    identity
        .integrations
        .insert("continuum".to_string(), name.clone());
    save_identity_card(home, identity).await?;
    println!(
        "  imported continuum:{} -> pronouns={} role={} bio set={}",
        if name.is_empty() { "?" } else { &name },
        source.get("pronouns").and_then(Value::as_str).unwrap_or(""),
        source.get("role").and_then(Value::as_str).unwrap_or(""),
        source
            .get("bio")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .is_some()
    );
    Ok(())
}

pub async fn run_continuum_handle(home: &Path) -> Result<(), Box<dyn Error>> {
    let identity = load_identity_card(home).await?;
    if let Some(handle) = identity
        .integrations
        .get("continuum")
        .filter(|value| !value.is_empty())
    {
        println!("{handle}");
    }
    Ok(())
}

pub async fn run_push_continuum(home: &Path, handle: &str) -> Result<(), Box<dyn Error>> {
    let identity = load_identity_card(home).await?;
    let mut args = vec![
        "persona".to_string(),
        "update".to_string(),
        handle.to_string(),
    ];
    for (key, value) in [
        ("pronouns", identity.pronouns),
        ("role", identity.role),
        ("bio", identity.bio),
    ] {
        if !value.is_empty() {
            args.push(format!("--{key}"));
            args.push(value);
        }
    }
    let output = Command::new("continuum").args(&args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = stderr.trim();
        let stdout = stdout.trim();
        let message = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!("  continuum push failed: {}", message).into());
    }
    println!("  pushed local identity to continuum:{handle}");
    Ok(())
}

async fn load_identity_card(home: &Path) -> Result<Identity, Box<dyn Error>> {
    let _ = LocalIdentity::load_or_generate(home).await?;
    let store = identity_store(home).await?;
    let stored = store
        .load_local_identity()
        .await?
        .ok_or("local identity row missing after initialization")?;
    Ok(stored.identity)
}

async fn save_identity_card(home: &Path, identity: Identity) -> Result<(), Box<dyn Error>> {
    let _ = LocalIdentity::load_or_generate(home).await?;
    let store = identity_store(home).await?;
    store.save_local_identity_card(identity).await?;
    Ok(())
}

async fn identity_store(home: &Path) -> Result<Box<dyn EventStore>, Box<dyn Error>> {
    Ok(Box::new(
        SqliteEventStore::open_path(&home.join("events.sqlite")).await?,
    ))
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

#[derive(Debug)]
struct NudgeNeeded;

impl std::fmt::Display for NudgeNeeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "identity nudge needed")
    }
}

impl Error for NudgeNeeded {}

pub fn command_exit_code(error: &(dyn Error + 'static)) -> Option<u8> {
    if error.is::<NudgeNeeded>() {
        Some(2)
    } else {
        None
    }
}

fn session_file(write_dir: &Path, transport_name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let (source, value) = session_source();
    let digest = sha256_hex(format!("{source}:{value}").as_bytes());
    let _ = transport_name;
    Ok(write_dir
        .join("sessions")
        .join(format!("{}.json", &digest[..16])))
}

fn default_work_name(transport_name: &str, session_file: &Path) -> String {
    let digest = sha256_hex(session_file.display().to_string().as_bytes());
    format!("{}-{}", slug(transport_name), &digest[..4])
}

fn read_work_name(session_file: &Path) -> Option<String> {
    fs::read_to_string(session_file)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

fn write_work_session(
    session_file: &Path,
    name: &str,
    transport_name: &str,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = session_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let (source, value) = session_source();
    let mut root = fs::read_to_string(session_file)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| Value::Object(Default::default()));
    let object = root
        .as_object_mut()
        .ok_or("session file root must be a JSON object")?;
    object.insert("name".to_string(), Value::String(name.to_string()));
    object.insert(
        "transport_name".to_string(),
        Value::String(transport_name.to_string()),
    );
    object.insert("session_source".to_string(), Value::String(source));
    object.insert("session_hint".to_string(), Value::String(value));
    object.insert(
        "updated_at".to_string(),
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)),
    );
    fs::write(session_file, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn session_source() -> (String, String) {
    for key in [
        "AIRC_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
        "TERM_SESSION_ID",
        "TMUX_PANE",
    ] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return (key.to_string(), value.to_string());
            }
        }
    }
    ("cwd".to_string(), current_dir_string())
}

fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "agent".to_string()
    } else {
        out.to_string()
    }
}

fn set_optional_string(field: &mut String, value: Option<String>) {
    if let Some(value) = value {
        let value = value.trim();
        if value.is_empty() {
            field.clear();
        } else {
            *field = value.to_string();
        }
    }
}

fn set_from_source(field: &mut String, source: &Value, key: &str) {
    if let Some(value) = source
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        *field = value.to_string();
    }
}

fn validate_len(name: &str, value: Option<&str>, max: usize) -> Result<(), Box<dyn Error>> {
    if let Some(value) = value.filter(|value| value.len() > max) {
        return Err(format!("{name} too long ({} chars; max {max})", value.len()).into());
    }
    Ok(())
}

fn print_identity(identity: &Identity) {
    print_identity_value("name", &identity.name, "");
    print_identity_value(
        "pronouns",
        &truncate_str(&identity.pronouns, PRONOUNS_MAX),
        "(unset)",
    );
    print_identity_value("role", &truncate_str(&identity.role, ROLE_MAX), "(unset)");
    print_identity_value("bio", &truncate_str(&identity.bio, BIO_MAX), "(unset)");
    print_identity_value(
        "status",
        &truncate_str(&identity.status, STATUS_MAX),
        "(unset; airc away <msg> to set)",
    );
    if !identity.integrations.is_empty() {
        println!("  integrations:");
        for (key, value) in &identity.integrations {
            println!("    {key}: {value}");
        }
    } else {
        println!("  integrations: (none)");
    }
}

fn print_identity_value(key: &str, value: &str, fallback: &str) {
    let label = format!("{key}:");
    let display = if value.is_empty() { fallback } else { value };
    println!("  {label:<11} {display}");
}

fn truncate_str(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        let prefix: String = value.chars().take(max.saturating_sub(3)).collect();
        format!("{prefix}...")
    }
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

    #[test]
    fn work_session_round_trips_saved_name() {
        temp_env::with_var("AIRC_SESSION_ID", Some("thread-123"), || {
            let dir = tempfile::tempdir().unwrap();
            let session = session_file(dir.path(), "Codex Agent").unwrap();

            assert!(session.ends_with("sessions/1700b5a26accf9e8.json"));
            assert!(default_work_name("Codex Agent", &session).starts_with("codex-agent-"));
            assert_eq!(read_work_name(&session), None);

            write_work_session(&session, "codex-tab-1", "Codex Agent").unwrap();

            assert_eq!(read_work_name(&session).as_deref(), Some("codex-tab-1"));
            let saved: Value = serde_json::from_str(&fs::read_to_string(session).unwrap()).unwrap();
            assert_eq!(saved["transport_name"], "Codex Agent");
            assert_eq!(saved["session_source"], "AIRC_SESSION_ID");
            assert_eq!(saved["session_hint"], "thread-123");
        });
    }

    #[test]
    fn truncated_preserves_utf8_boundary() {
        assert_eq!(truncate_str("ééééé", 4), "é...");
    }
}
