use std::env;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde_json::Value;

const DEFAULT_MAX_REQUESTS_PER_MIN: usize = 30;

pub(crate) fn reserve_guarded_request(
    args: &[String],
    now: f64,
) -> Result<(bool, String), Box<dyn Error>> {
    let _lock = GuardLock::acquire()?;
    let until = backoff_until();
    if now < until {
        return Ok((
            false,
            format!("shared backoff active for {}s", (until - now) as i64),
        ));
    }
    let count = recent_request_count(now)?;
    let limit = max_requests_per_min();
    // Starvation fix (task #288, 2026-08-01), mirroring
    // airc-lib::gh::governor::reserve_class — this CLI path hosts the
    // BEACON/channel polling traffic, so it is capped below the shared
    // limit to leave airc-lib's REGISTRY_FLOOR untouchable: registry
    // convergence must always find budget no matter how hot polling
    // runs (the 33k-error empty-registry incident). And a LOCAL exceed
    // no longer arms the SHARED backoff — that let one noisy poller
    // lock out every caller (registry included) for 60s repeatedly;
    // the sliding window is already self-limiting, and the shared
    // backoff stays exclusively GitHub's own voice (note_rate_limit).
    // Eventual single owner: delegate this whole fn to the lib
    // governor (two implementations over one budget file is the
    // registry_bridge smell).
    let cap = limit.saturating_sub(airc_lib::gh::governor::REGISTRY_FLOOR);
    // SOS is the channel of last resort: it is what a human or agent
    // reaches for precisely when the wire is down and the poller is hot.
    // Starving it behind the same bucket as beacon/channel polling meant
    // "airc sos watch" answered "budget exceeded" during the exact
    // incident it exists to coordinate — observed live 2026-08-13, and
    // it is why an operator concluded the emergency channel was dead.
    // It gets its own reserve above the poller cap, mirroring the
    // REGISTRY_FLOOR carve-out one tier down.
    let effective_cap = if is_sos_request(args) {
        cap + SOS_RESERVE
    } else {
        cap
    };
    if count >= effective_cap {
        // Report when the LOCAL window actually frees, not the shared
        // backoff. `wait_seconds` reads GitHub's voice, which is 0 here —
        // so the old message told the caller "retry in 0s" while refusing
        // them, and a retry loop honoring it would spin at full speed
        // against the very limiter that just said no.
        return Ok((
            false,
            format!(
                "local request budget exceeded ({count}/{effective_cap} of {limit} in 60s); \
                 window frees in {}s",
                local_window_frees_in(now)?
            ),
        ));
    }
    if guarded_command(args) {
        record_request(now)?;
    }
    Ok((true, "allowed".to_string()))
}

pub(crate) fn budget_snapshot(now: f64) -> Result<(usize, usize), Box<dyn Error>> {
    let _lock = GuardLock::acquire()?;
    Ok((recent_request_count(now)?, max_requests_per_min()))
}

struct GuardLock(File);

impl GuardLock {
    fn acquire() -> Result<Self, Box<dyn Error>> {
        let path = lock_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        file.lock_exclusive()?;
        Ok(Self(file))
    }
}

impl Drop for GuardLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
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

/// Extra 60s-window slots reserved for `airc sos`, ON TOP of the poller
/// cap. Small on purpose: SOS is low-rate human/agent coordination, not a
/// poller, so a handful of slots is the difference between "the emergency
/// channel answers" and "the emergency channel is dead".
const SOS_RESERVE: usize = 6;

/// True when this gh invocation is SOS traffic (the gist-comment channel
/// of last resort). Matched on the request itself rather than threaded
/// through as a flag so no future caller can forget to mark it.
fn is_sos_request(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg.contains("/comments") || arg == "rate_limit")
}

/// Seconds until the OLDEST request in the sliding window ages out — i.e.
/// when a slot actually frees. Returns 1 rather than 0 when the window is
/// somehow empty, because a refusal must never advertise "retry now".
fn local_window_frees_in(now: f64) -> std::io::Result<i64> {
    let oldest = fs::read_to_string(budget_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse::<f64>().ok())
        .filter(|ts| *ts >= now - 60.0)
        .fold(f64::INFINITY, f64::min);
    if !oldest.is_finite() {
        return Ok(1);
    }
    Ok(((oldest + 60.0 - now).ceil() as i64).max(1))
}

fn recent_request_count(now: f64) -> std::io::Result<usize> {
    let path = budget_path();
    let cutoff = now - 60.0;
    let kept: Vec<f64> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse::<f64>().ok())
        .filter(|ts| *ts >= cutoff)
        .collect();
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = File::create(&tmp)?;
    for ts in &kept {
        writeln!(file, "{ts:.3}")?;
    }
    fs::rename(tmp, path)?;
    Ok(kept.len())
}

fn record_request(now: f64) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(budget_path())?;
    writeln!(file, "{now:.3}")
}

fn max_requests_per_min() -> usize {
    env::var("AIRC_GH_MAX_REQUESTS_PER_MIN")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_REQUESTS_PER_MIN)
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

fn lock_path() -> PathBuf {
    gh_state_dir().join("guard.lock")
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
