use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::gh_state::{
    backoff_until, now_seconds, record_backoff, reserve_guarded_request, split_include_output,
};

const GIST_LIST_LIMIT: usize = 100;

pub fn run_find(channel: &str, require_invite: bool) -> Result<(), Box<dyn Error>> {
    if let Some(gist_id) = find_existing(channel, require_invite)? {
        println!("{gist_id}");
        Ok(())
    } else {
        Err("channel gist not found".into())
    }
}

pub fn run_host_preflight(channel: &str, config: Option<&Path>) -> Result<(), Box<dyn Error>> {
    if let Some(gist_id) = config_channel_gist(config, channel) {
        println!("{gist_id}");
        return Ok(());
    }
    match find_existing_with_state(channel, false)? {
        Discovery::Found(gist_id) => {
            println!("{gist_id}");
            Ok(())
        }
        Discovery::Unavailable => Err(ExitCodeError(2).into()),
        Discovery::Missing => Err(ExitCodeError(1).into()),
    }
}

fn find_existing(channel: &str, require_invite: bool) -> Result<Option<String>, Box<dyn Error>> {
    Ok(match find_existing_with_state(channel, require_invite)? {
        Discovery::Found(gist_id) => Some(gist_id),
        Discovery::Missing | Discovery::Unavailable => None,
    })
}

enum Discovery {
    Found(String),
    Missing,
    Unavailable,
}

fn find_existing_with_state(
    channel: &str,
    require_invite: bool,
) -> Result<Discovery, Box<dyn Error>> {
    let Some(gists) = list_user_gists()? else {
        return Ok(Discovery::Unavailable);
    };
    let candidates: Vec<Value> = gists
        .into_iter()
        .filter(|gist| {
            let desc = gist
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            desc.starts_with("airc mesh") || desc.starts_with("airc room:")
        })
        .collect();

    if let Some(gist_id) = choose_canonical(
        candidates
            .iter()
            .filter(|gist| is_single_channel_match(gist, channel, require_invite)),
        channel,
    ) {
        return Ok(Discovery::Found(gist_id));
    }

    let mut deep_canonical = Vec::new();
    for gist in &candidates {
        let Some(gist_id) = gist.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(mut full) = get_gist(gist_id)? {
            if is_single_channel_match(&full, channel, require_invite) {
                carry_timestamp(gist, &mut full);
                deep_canonical.push(full);
            }
        }
    }
    if let Some(gist_id) = choose_canonical(deep_canonical.iter(), channel) {
        return Ok(Discovery::Found(gist_id));
    }

    if let Some(gist_id) = oldest(
        candidates
            .iter()
            .filter(|gist| gist_describes_channel(gist, channel, require_invite)),
    ) {
        return Ok(Discovery::Found(gist_id));
    }

    let mut deep_legacy = Vec::new();
    for gist in &candidates {
        let Some(gist_id) = gist.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(mut full) = get_gist(gist_id)? {
            if gist_describes_channel(&full, channel, require_invite) {
                carry_timestamp(gist, &mut full);
                deep_legacy.push(full);
            }
        }
    }
    if let Some(gist_id) = oldest(deep_legacy.iter()) {
        return Ok(Discovery::Found(gist_id));
    }
    Ok(Discovery::Missing)
}

fn list_user_gists() -> Result<Option<Vec<Value>>, Box<dyn Error>> {
    let fresh_sec = env_f64("AIRC_GIST_LIST_CACHE_SEC", 60.0);
    if let Some(cached) = load_cached_gist_list(fresh_sec) {
        return Ok(Some(cached));
    }
    if backoff_until() > now_seconds() {
        return Ok(load_cached_gist_list(env_f64(
            "AIRC_GIST_LIST_STALE_SEC",
            900.0,
        )));
    }
    let args = vec![
        "api".to_string(),
        "--include".to_string(),
        format!("gists?per_page={GIST_LIST_LIMIT}"),
    ];
    let (allowed, _) = reserve_guarded_request(&args, now_seconds())?;
    if !allowed {
        return Ok(load_cached_gist_list(env_f64(
            "AIRC_GIST_LIST_STALE_SEC",
            900.0,
        )));
    }
    let gh = env::var("AIRC_GH_BIN").unwrap_or_else(|_| "gh".to_string());
    let Ok(output) = Command::new(gh).args(&args).output() else {
        return Ok(load_cached_gist_list(env_f64(
            "AIRC_GIST_LIST_STALE_SEC",
            900.0,
        )));
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        record_backoff(&format!("{stderr}{stdout}"));
        return Ok(load_cached_gist_list(env_f64(
            "AIRC_GIST_LIST_STALE_SEC",
            900.0,
        )));
    }
    let (headers, body) = split_include_output(&stdout);
    record_backoff(&headers);
    let loaded: Value = serde_json::from_str(body.trim())?;
    let Some(items) = loaded.as_array() else {
        return Ok(None);
    };
    let gists = items.clone();
    save_cached_gist_list(&gists);
    Ok(Some(gists))
}

fn get_gist(gist_id: &str) -> Result<Option<Value>, Box<dyn Error>> {
    if backoff_until() > now_seconds() {
        return Ok(None);
    }
    let args = vec![
        "api".to_string(),
        "--include".to_string(),
        format!("gists/{gist_id}"),
    ];
    let (allowed, _) = reserve_guarded_request(&args, now_seconds())?;
    if !allowed {
        return Ok(None);
    }
    let gh = env::var("AIRC_GH_BIN").unwrap_or_else(|_| "gh".to_string());
    let Ok(output) = Command::new(gh).args(&args).output() else {
        return Ok(None);
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        record_backoff(&format!("{stderr}{stdout}"));
        return Ok(None);
    }
    let (headers, body) = split_include_output(&stdout);
    record_backoff(&headers);
    Ok(serde_json::from_str(body.trim()).ok())
}

fn choose_canonical<'a>(matches: impl Iterator<Item = &'a Value>, channel: &str) -> Option<String> {
    let matches: Vec<&Value> = matches.collect();
    let prefix = format!("airc room: #{channel}");
    oldest(matches.iter().copied().filter(|gist| {
        gist.get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .starts_with(&prefix)
    }))
    .or_else(|| oldest(matches.into_iter()))
}

fn oldest<'a>(matches: impl Iterator<Item = &'a Value>) -> Option<String> {
    matches
        .filter_map(|gist| {
            let id = gist.get("id")?.as_str()?.to_string();
            let created = gist
                .get("created_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((created, id))
        })
        .min_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)))
        .map(|(_, id)| id)
}

fn is_single_channel_match(gist: &Value, channel: &str, require_invite: bool) -> bool {
    let exact_name = format!("airc-room-{channel}.json");
    gist.get("files")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|files| files.iter())
        .any(|(name, entry)| {
            let Some(envelope) = entry
                .get("content")
                .and_then(Value::as_str)
                .and_then(|content| serde_json::from_str::<Value>(content).ok())
            else {
                return false;
            };
            if require_invite
                && !envelope
                    .get("invite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                return false;
            }
            let channels = envelope.get("channels").and_then(Value::as_array);
            (name == &exact_name
                && channels
                    .into_iter()
                    .flatten()
                    .any(|item| item.as_str() == Some(channel)))
                || channels
                    .map(|items| items.len() == 1 && items[0].as_str() == Some(channel))
                    .unwrap_or(false)
        })
}

fn gist_describes_channel(gist: &Value, channel: &str, require_invite: bool) -> bool {
    gist.get("files")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|files| files.values())
        .any(|entry| {
            let Some(envelope) = entry
                .get("content")
                .and_then(Value::as_str)
                .and_then(|content| serde_json::from_str::<Value>(content).ok())
            else {
                return false;
            };
            if require_invite
                && !envelope
                    .get("invite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                return false;
            }
            envelope
                .get("channels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|item| item.as_str() == Some(channel))
        })
}

fn carry_timestamp(source: &Value, target: &mut Value) {
    if let Some(created_at) = source.get("created_at").cloned() {
        target["created_at"] = created_at;
    }
}

fn config_channel_gist(config: Option<&Path>, channel: &str) -> Option<String> {
    let path = config?;
    let raw = fs::read_to_string(path).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    let gist_id = root
        .get("channel_gists")?
        .get(channel)?
        .as_str()?
        .to_string();
    valid_gist_id(&gist_id).then_some(gist_id)
}

fn valid_gist_id(gist_id: &str) -> bool {
    (8..=64).contains(&gist_id.len()) && gist_id.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn cache_path() -> PathBuf {
    let suffix = state_suffix();
    env::temp_dir().join(format!("airc-gh-gist-list-{suffix}.json"))
}

fn state_suffix() -> String {
    #[cfg(unix)]
    {
        unsafe { libc::getuid().to_string() }
    }
    #[cfg(not(unix))]
    {
        env::var("USERNAME").unwrap_or_else(|_| "user".to_string())
    }
}

fn load_cached_gist_list(max_age: f64) -> Option<Vec<Value>> {
    let path = cache_path();
    let age = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(|elapsed| elapsed.as_secs_f64())?;
    if age > max_age {
        return None;
    }
    serde_json::from_str::<Vec<Value>>(&fs::read_to_string(path).ok()?).ok()
}

fn save_cached_gist_list(gists: &[Value]) {
    let path = cache_path();
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if fs::write(&tmp, serde_json::to_vec(gists).unwrap_or_default()).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[derive(Debug)]
struct ExitCodeError(u8);

impl std::fmt::Display for ExitCodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "exit {}", self.0)
    }
}

impl Error for ExitCodeError {}

pub fn command_exit_code(error: &(dyn Error + 'static)) -> Option<u8> {
    error.downcast_ref::<ExitCodeError>().map(|error| error.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn single_channel_exact_file_matches() {
        let gist = json!({
            "files": {
                "airc-room-general.json": {"content": r#"{"channels":["general"]}"#}
            }
        });

        assert!(is_single_channel_match(&gist, "general", false));
    }

    #[test]
    fn require_invite_rejects_uninvited_gist() {
        let gist = json!({
            "files": {
                "airc-room-general.json": {"content": r#"{"channels":["general"]}"#}
            }
        });

        assert!(!is_single_channel_match(&gist, "general", true));
    }

    #[test]
    fn canonical_description_wins_before_oldest_fallback() {
        let generic = json!({
            "id": "bbbbbbbb",
            "description": "airc mesh",
            "created_at": "2026-01-01T00:00:00Z"
        });
        let room = json!({
            "id": "aaaaaaaa",
            "description": "airc room: #general",
            "created_at": "2026-02-01T00:00:00Z"
        });

        assert_eq!(
            choose_canonical([generic, room].iter(), "general"),
            Some("aaaaaaaa".to_string())
        );
    }
}
