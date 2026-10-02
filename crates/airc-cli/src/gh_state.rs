use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use airc_lib::gh::governor::{GhBudget, GhClass, Reservation};
use serde_json::Value;

pub(crate) fn reserve_guarded_request(
    args: &[String],
    now: f64,
) -> Result<(bool, String), Box<dyn Error>> {
    reserve_request(args, now, GhClass::Beacon)
}

pub(crate) fn reserve_recovery_request(
    args: &[String],
    now: f64,
) -> Result<(bool, String), Box<dyn Error>> {
    reserve_request(args, now, GhClass::Recovery)
}

fn reserve_request(
    args: &[String],
    now: f64,
    class: GhClass,
) -> Result<(bool, String), Box<dyn Error>> {
    match GhBudget::account_default().reserve_class(args, now, class)? {
        Reservation::Allowed => Ok((true, "allowed".into())),
        Reservation::Denied {
            retry_after_secs,
            reason,
        } => Ok((false, format!("{reason}; retry in {retry_after_secs}s"))),
    }
}

pub(crate) fn budget_snapshot(now: f64) -> Result<(usize, usize), Box<dyn Error>> {
    Ok(GhBudget::account_default().snapshot(now)?)
}
pub(crate) fn wait_seconds(now: f64) -> i64 {
    (backoff_until() - now).max(0.0) as i64
}

pub(crate) fn backoff_until() -> f64 {
    fs::read_to_string(backoff_path())
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
}

pub(crate) fn record_backoff(output: &str) {
    let body = output.to_ascii_lowercase();
    if body.is_empty() {
        return;
    }
    let now = now_seconds();
    let mut until = 0.0;
    if let Some(retry) =
        header_value(&body, "retry-after").and_then(|value| value.parse::<f64>().ok())
    {
        until = now + retry.max(1.0);
    } else {
        let remaining = header_value(&body, "x-ratelimit-remaining");
        let reset =
            header_value(&body, "x-ratelimit-reset").and_then(|value| value.parse::<f64>().ok());
        if remaining.as_deref() == Some("0") {
            if let Some(reset) = reset {
                until = reset;
            }
        } else if body.contains("secondary rate limit")
            || body.contains("rate limit exceeded")
            || body.contains("abuse detection")
        {
            until = now + 60.0;
        }
    }
    if until > now {
        let _ = write_backoff(until);
    }
}

fn header_value(body: &str, name: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == name).then(|| value.trim().to_string())
    })
}

fn write_backoff(until: f64) -> std::io::Result<()> {
    if until <= now_seconds() {
        return Ok(());
    }
    let until = until.max(backoff_until());
    let path = backoff_path();
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, format!("{}", until as i64))?;
    fs::rename(tmp, path)
}

pub(crate) fn guarded_command(args: &[String]) -> bool {
    matches!(args.first().map(String::as_str), Some("api" | "gist"))
        || matches!(
            (
                args.first().map(String::as_str),
                args.get(1).map(String::as_str)
            ),
            (Some("auth"), Some("status"))
        )
}

pub(crate) fn command_class(args: &[String]) -> String {
    match args {
        [] => "unknown".to_string(),
        [first, rest @ ..] if first == "api" => rest
            .iter()
            .find(|part| !part.starts_with('-'))
            .map(|part| format!("api:{}", part.split('?').next().unwrap_or(part)))
            .unwrap_or_else(|| "api".to_string()),
        [first, second, ..] if first == "gist" => format!("gist:{second}"),
        [first, second, ..] if first == "auth" => format!("auth:{second}"),
        [first, ..] => first.clone(),
    }
}

pub(crate) fn safe_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut redact_next = false;
    for arg in args {
        if redact_next {
            out.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }
        if matches!(
            arg.as_str(),
            "--input" | "-F" | "--field" | "-f" | "--raw-field"
        ) {
            out.push(arg.clone());
            if arg != "--input" {
                redact_next = true;
            }
            continue;
        }
        if arg.to_ascii_lowercase().contains("token")
            || arg.to_ascii_lowercase().contains("authorization:")
        {
            out.push("<redacted>".to_string());
        } else {
            out.push(arg.chars().take(180).collect());
        }
    }
    out
}

pub(crate) fn split_include_output(raw: &str) -> (String, String) {
    let normalized = raw.replace("\r\n", "\n");
    if normalized.starts_with("HTTP/") {
        if let Some(index) = normalized.find("\n\n") {
            let (headers, body) = normalized.split_at(index);
            return (headers.to_string(), body.trim_start().to_string());
        }
    }
    (String::new(), raw.to_string())
}

pub(crate) fn append_audit(event: &Value) {
    let path = audit_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if path
        .metadata()
        .map(|meta| meta.len() > audit_max_bytes())
        .unwrap_or(false)
    {
        let rotated = path.with_extension("jsonl.1");
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(&path, rotated);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", serde_json::to_string(event).unwrap_or_default());
    }
}

pub(crate) fn recent_events(count: usize) -> Vec<Value> {
    let mut rows: Vec<Value> = fs::read_to_string(audit_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    if rows.len() > count {
        rows.drain(0..rows.len() - count);
    }
    rows
}

fn audit_max_bytes() -> u64 {
    env::var("AIRC_GH_AUDIT_MAX_BYTES")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(262_144)
}

pub(crate) fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

pub(crate) fn cwd() -> String {
    env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

/// Per-account GitHub guard state lives under the machine-account home
/// (`~/.airc/gh/`), not a temp dir — same "all state under `.airc`"
/// discipline as the daemon socket. `.airc` is already per-user, so no
/// uid prefix is needed. The directory is created on demand.
fn gh_state_dir() -> PathBuf {
    let account_home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".airc"))
        // No home env at all (headless/broken env): fall back to a temp
        // `.airc` so the guard still functions rather than panicking.
        .unwrap_or_else(|| env::temp_dir().join(".airc"));
    let dir = account_home.join("gh");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub(crate) fn backoff_path() -> PathBuf {
    gh_state_dir().join("backoff-until")
}

pub(crate) fn budget_path() -> PathBuf {
    gh_state_dir().join("budget.jsonl")
}

pub(crate) fn audit_path() -> PathBuf {
    env::var_os("AIRC_GH_AUDIT_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| gh_state_dir().join("requests.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_classes_match_legacy_shapes() {
        assert_eq!(
            command_class(&["api".into(), "/rate_limit".into()]),
            "api:/rate_limit"
        );
        assert_eq!(command_class(&["gist".into(), "edit".into()]), "gist:edit");
        assert_eq!(
            command_class(&["auth".into(), "status".into()]),
            "auth:status"
        );
    }

    #[test]
    fn safe_args_redacts_token_like_values() {
        let args = safe_args(&["api".into(), "--raw-field".into(), "token=abc".into()]);
        assert_eq!(args, vec!["api", "--raw-field", "<redacted>"]);
    }
}
