use std::collections::{BTreeSet, VecDeque};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

pub fn run_rename_collision(
    messages_file: &Path,
    target: &str,
    old_name: &str,
    tail_lines: usize,
) -> Result<(), Box<dyn Error>> {
    if rename_collision(messages_file, target, old_name, tail_lines) {
        Ok(())
    } else {
        Err(NoCollision.into())
    }
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

pub fn run_show_config(config: &Path) -> Result<(), Box<dyn Error>> {
    let Some(root) = load_config_opt(config) else {
        println!("  (no config — run airc join)");
        return Ok(());
    };
    print_config_identity(&root);
    Ok(())
}

pub fn run_set_config(
    config: &Path,
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

    let mut root = load_config_required(config)?;
    let ident = object_field_mut(&mut root, "identity")?;
    set_optional_string(ident, "pronouns", pronouns);
    set_optional_string(ident, "role", role);
    set_optional_string(ident, "bio", bio);
    set_optional_string(ident, "status", status);
    save_config(config, &root)?;
    println!("  identity updated.");
    Ok(())
}

pub fn run_link_config(config: &Path, platform: &str, handle: &str) -> Result<(), Box<dyn Error>> {
    let mut root = load_config_required(config)?;
    let ident = object_field_mut(&mut root, "identity")?;
    let integrations = object_map_field_mut(ident, "integrations")?;
    let previous = integrations
        .get(platform)
        .and_then(Value::as_str)
        .map(str::to_owned);
    if handle.trim().is_empty() {
        integrations.remove(platform);
    } else {
        integrations.insert(
            platform.to_string(),
            Value::String(handle.trim().to_string()),
        );
    }
    save_config(config, &root)?;

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

pub fn run_nudge_needed(config: &Path) -> Result<(), Box<dyn Error>> {
    let Some(root) = load_config_opt(config) else {
        return Ok(());
    };
    let ident = root.get("identity").and_then(Value::as_object);
    let all_unset = ["pronouns", "role", "bio"].iter().all(|field| {
        ident
            .and_then(|items| items.get(*field))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .is_none()
    });
    if all_unset {
        return Err(NudgeNeeded.into());
    }
    Ok(())
}

pub fn run_import_continuum(config: &Path, blob: &str) -> Result<(), Box<dyn Error>> {
    let source: Value = serde_json::from_str(blob).unwrap_or(Value::Null);
    let mut root = load_config_required(config)?;
    let ident = object_field_mut(&mut root, "identity")?;
    for key in ["pronouns", "role", "bio"] {
        if let Some(value) = source
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            ident.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
    let name = source
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let integrations = object_map_field_mut(ident, "integrations")?;
    integrations.insert("continuum".to_string(), Value::String(name.clone()));
    save_config(config, &root)?;
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

pub fn run_continuum_handle(config: &Path) -> Result<(), Box<dyn Error>> {
    let root = load_config_required(config)?;
    if let Some(handle) = root
        .get("identity")
        .and_then(|identity| identity.get("integrations"))
        .and_then(|integrations| integrations.get("continuum"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        println!("{handle}");
    }
    Ok(())
}

pub fn run_push_continuum(config: &Path, handle: &str) -> Result<(), Box<dyn Error>> {
    let root = load_config_required(config)?;
    let identity = root.get("identity").unwrap_or(&Value::Null);
    let mut args = vec![
        "persona".to_string(),
        "update".to_string(),
        handle.to_string(),
    ];
    for key in ["pronouns", "role", "bio"] {
        if let Some(value) = identity
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            args.push(format!("--{key}"));
            args.push(value.to_string());
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

#[derive(Debug)]
struct NoCollision;

impl std::fmt::Display for NudgeNeeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "identity nudge needed")
    }
}

impl Error for NudgeNeeded {}

impl std::fmt::Display for NoCollision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "no rename collision")
    }
}

impl Error for NoCollision {}

pub fn command_exit_code(error: &(dyn Error + 'static)) -> Option<u8> {
    if error.is::<NudgeNeeded>() {
        Some(2)
    } else if error.is::<NoCollision>() {
        Some(1)
    } else {
        None
    }
}

fn rename_collision(messages_file: &Path, target: &str, old_name: &str, tail_lines: usize) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    let mut seen = BTreeSet::new();
    let mut my_history = BTreeSet::from([old_name.trim().to_string()]);
    for line in tail_lines_from_file(messages_file, tail_lines) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(sender) = message
            .get("from")
            .and_then(Value::as_str)
            .filter(|sender| !sender.is_empty())
        {
            seen.insert(sender.to_string());
        }
        if let Some((old, new)) = message
            .get("msg")
            .and_then(Value::as_str)
            .and_then(parse_rename_marker)
        {
            if my_history.contains(old) || my_history.contains(new) {
                my_history.insert(old.to_string());
                my_history.insert(new.to_string());
            }
        }
    }
    seen.contains(target) && !my_history.contains(target)
}

fn tail_lines_from_file(path: &Path, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut lines = VecDeque::with_capacity(limit);
    for line in fs::read_to_string(path).unwrap_or_default().lines() {
        if lines.len() == limit {
            lines.pop_front();
        }
        lines.push_back(line.to_string());
    }
    lines.into_iter().collect()
}

fn parse_rename_marker(message: &str) -> Option<(&str, &str)> {
    let rest = message.strip_prefix("[rename] ")?;
    let mut old = None;
    let mut new = None;
    for part in rest.split_whitespace() {
        if let Some(value) = part.strip_prefix("old=") {
            old = Some(value);
        } else if let Some(value) = part.strip_prefix("new=") {
            new = Some(value);
        }
    }
    Some((old?, new?))
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

fn load_config_opt(config: &Path) -> Option<Value> {
    fs::read_to_string(config)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
}

fn load_config_required(config: &Path) -> Result<Value, Box<dyn Error>> {
    let root = load_config_opt(config).ok_or("config JSON is missing or malformed")?;
    if !root.is_object() {
        return Err("config JSON root must be an object".into());
    }
    Ok(root)
}

fn save_config(config: &Path, root: &Value) -> Result<(), Box<dyn Error>> {
    fs::write(config, serde_json::to_string_pretty(root)?)?;
    Ok(())
}

fn object_field_mut<'a>(
    root: &'a mut Value,
    key: &str,
) -> Result<&'a mut serde_json::Map<String, Value>, Box<dyn Error>> {
    let Value::Object(object) = root else {
        return Err("config JSON root must be an object".into());
    };
    if !object.get(key).is_some_and(Value::is_object) {
        object.insert(key.to_string(), Value::Object(Default::default()));
    }
    match object.get_mut(key) {
        Some(Value::Object(field)) => Ok(field),
        Some(
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_),
        )
        | None => Err(format!("failed to initialize {key} object").into()),
    }
}

fn object_map_field_mut<'a>(
    object: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a mut serde_json::Map<String, Value>, Box<dyn Error>> {
    if !object.get(key).is_some_and(Value::is_object) {
        object.insert(key.to_string(), Value::Object(Default::default()));
    }
    match object.get_mut(key) {
        Some(Value::Object(field)) => Ok(field),
        Some(
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_),
        )
        | None => Err(format!("failed to initialize {key} object").into()),
    }
}

fn set_optional_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        let value = value.trim();
        if value.is_empty() {
            object.remove(key);
        } else {
            object.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
}

fn validate_len(name: &str, value: Option<&str>, max: usize) -> Result<(), Box<dyn Error>> {
    if let Some(value) = value.filter(|value| value.len() > max) {
        return Err(format!("{name} too long ({} chars; max {max})", value.len()).into());
    }
    Ok(())
}

fn print_config_identity(root: &Value) {
    let identity = root.get("identity").unwrap_or(&Value::Null);
    let name = root.get("name").and_then(Value::as_str).unwrap_or("?");
    print_identity_value("name", name, "");
    print_identity_field(identity, "pronouns", PRONOUNS_MAX, "(unset)");
    print_identity_field(identity, "role", ROLE_MAX, "(unset)");
    print_identity_field(identity, "bio", BIO_MAX, "(unset)");
    print_identity_field(
        identity,
        "status",
        STATUS_MAX,
        "(unset; airc away <msg> to set)",
    );
    if let Some(integrations) = identity
        .get("integrations")
        .and_then(Value::as_object)
        .filter(|items| !items.is_empty())
    {
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
}

fn print_identity_field(identity: &Value, key: &str, max: usize, fallback: &str) {
    let value = truncated(identity, key, max).unwrap_or_default();
    print_identity_value(key, &value, fallback);
}

fn print_identity_value(key: &str, value: &str, fallback: &str) {
    let label = format!("{key}:");
    let display = if value.is_empty() { fallback } else { value };
    println!("  {label:<11} {display}");
}

fn truncated(identity: &Value, key: &str, max: usize) -> Option<String> {
    let value = identity.get(key)?.as_str()?.to_string();
    if value.chars().count() <= max {
        Some(value)
    } else {
        let prefix: String = value.chars().take(max.saturating_sub(3)).collect();
        Some(format!("{prefix}..."))
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
    fn rename_collision_detects_foreign_recent_sender() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("messages.jsonl");
        fs::write(
            &log,
            r#"{"from":"mine","msg":"hello","ts":"2026-05-20T00:00:00Z"}"#.to_string()
                + "\n"
                + r#"{"from":"alice","msg":"hello","ts":"2026-05-20T00:00:01Z"}"#
                + "\n",
        )
        .unwrap();

        assert!(rename_collision(&log, "alice", "mine", 200));
        assert!(!rename_collision(&log, "bob", "mine", 200));
    }

    #[test]
    fn rename_collision_excludes_local_rename_history() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("messages.jsonl");
        fs::write(
            &log,
            r#"{"from":"old","msg":"[rename] old=old new=temp","ts":"2026-05-20T00:00:00Z"}"#
                .to_string()
                + "\n"
                + r#"{"from":"temp","msg":"[rename] old=temp new=old","ts":"2026-05-20T00:00:01Z"}"#
                + "\n"
                + r#"{"from":"old","msg":"back","ts":"2026-05-20T00:00:02Z"}"#
                + "\n",
        )
        .unwrap();

        assert!(!rename_collision(&log, "temp", "old", 200));
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
    fn set_and_link_config_updates_identity() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(&config, r#"{"name":"codex","identity":{}}"#).unwrap();

        run_set_config(
            &config,
            Some("they".to_string()),
            Some("rust-cutter".to_string()),
            Some("Moves runtime identity state into Rust.".to_string()),
            None,
        )
        .unwrap();
        run_link_config(&config, "continuum", "clio").unwrap();

        let saved: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(saved["identity"]["pronouns"], "they");
        assert_eq!(saved["identity"]["role"], "rust-cutter");
        assert_eq!(
            saved["identity"]["bio"],
            "Moves runtime identity state into Rust."
        );
        assert_eq!(saved["identity"]["integrations"]["continuum"], "clio");

        run_link_config(&config, "continuum", "").unwrap();
        let saved: Value = serde_json::from_str(&fs::read_to_string(config).unwrap()).unwrap();
        assert!(saved["identity"]["integrations"]
            .as_object()
            .unwrap()
            .get("continuum")
            .is_none());
    }

    #[test]
    fn import_continuum_merges_identity_fields() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        fs::write(
            &config,
            r#"{"name":"codex","identity":{"status":"reviewing"}}"#,
        )
        .unwrap();

        run_import_continuum(
            &config,
            r#"{"name":"clio","pronouns":"she/they","role":"memory-architect","bio":"Builds recall."}"#,
        )
        .unwrap();

        let saved: Value = serde_json::from_str(&fs::read_to_string(config).unwrap()).unwrap();
        assert_eq!(saved["identity"]["pronouns"], "she/they");
        assert_eq!(saved["identity"]["role"], "memory-architect");
        assert_eq!(saved["identity"]["bio"], "Builds recall.");
        assert_eq!(saved["identity"]["status"], "reviewing");
        assert_eq!(saved["identity"]["integrations"]["continuum"], "clio");
    }

    #[test]
    fn truncated_preserves_utf8_boundary() {
        let value = json!({"bio":"ééééé"});

        assert_eq!(truncated(&value, "bio", 4).as_deref(), Some("é..."));
    }
}
