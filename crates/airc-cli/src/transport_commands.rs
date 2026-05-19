use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelHealth {
    channel: String,
    status: HealthStatus,
    age_seconds: Option<u64>,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthStatus {
    Ok,
    Degraded,
}

impl HealthStatus {
    fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "DEGRADED",
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    subscribed_channels: Vec<String>,
    #[serde(default)]
    channel_gists: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct BearerState {
    last_heartbeat_ts: Option<JsonNumber>,
    last_recv_ts: Option<JsonNumber>,
    last_error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum JsonNumber {
    Number(f64),
    String(String),
}

impl JsonNumber {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::String(value) => value.parse().ok(),
        }
    }
}

pub fn run_health(
    home: &Path,
    config: Option<PathBuf>,
    fresh_after: u64,
    quiet: bool,
    degraded_only: bool,
    fail: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = config.unwrap_or_else(|| home.join("config.json"));
    let rows = evaluate(home, &config, fresh_after, now_seconds());
    let degraded = rows
        .iter()
        .filter(|row| row.status == HealthStatus::Degraded)
        .count();

    if rows.is_empty() {
        return Err("transport health: no channel rows".into());
    }
    if degraded_only && degraded == 0 {
        return Ok(());
    }
    if quiet {
        return if degraded == 0 {
            Ok(())
        } else {
            Err("transport health degraded".into())
        };
    }

    if degraded == 0 {
        println!("transport health: ok ({} channel(s) fresh)", rows.len());
    } else {
        println!(
            "transport health: DEGRADED ({degraded}/{} channel(s) need attention)",
            rows.len()
        );
    }
    for row in rows {
        if degraded_only && row.status.is_ok() {
            continue;
        }
        let suffix = row
            .age_seconds
            .map(|age| format!("{age}s"))
            .unwrap_or_else(|| "no-signal".to_string());
        println!(
            "#{}: {} ({suffix}) - {}",
            row.channel,
            row.status.label(),
            row.detail
        );
    }

    if degraded == 0 || !fail {
        Ok(())
    } else {
        Err("transport health degraded".into())
    }
}

fn evaluate(home: &Path, config: &Path, fresh_after: u64, now: f64) -> Vec<ChannelHealth> {
    let config = load_config(config);
    let mut channels = config.subscribed_channels;
    if channels.is_empty() {
        channels = config.channel_gists.keys().cloned().collect();
    }

    channels
        .into_iter()
        .map(|channel| evaluate_channel(home, &config.channel_gists, &channel, fresh_after, now))
        .collect()
}

fn evaluate_channel(
    home: &Path,
    gists: &BTreeMap<String, String>,
    channel: &str,
    fresh_after: u64,
    now: f64,
) -> ChannelHealth {
    let gist = gists.get(channel).map(String::as_str).unwrap_or("");
    let mut issues = Vec::new();
    let mut age_seconds = None;

    if gist.is_empty() {
        issues.push("missing channel_gists mapping".to_string());
    }

    let state_path = home.join(format!("bearer_state.{channel}.json"));
    let state = load_state(&state_path);
    if let Some(error) = state.as_ref().and_then(|state| state.last_error.as_deref()) {
        issues.push(format!("bearer error: {error}"));
    }
    let signal = signal_for_gist(home, gists, channel, gist, state.as_ref());
    let signal_source = signal.as_ref().map(|(_, source)| source.clone());
    if let Some((signal_ts, _source)) = signal.as_ref() {
        if now >= *signal_ts {
            let age = (now - *signal_ts) as u64;
            age_seconds = Some(age);
            if age > fresh_after {
                issues.push(format!("stale heartbeat {age}s"));
            }
        } else {
            issues.push("invalid heartbeat timestamp".to_string());
        }
    } else if state.is_some() {
        match file_age_seconds(&state_path, now) {
            Some(age) if age <= fresh_after => {
                age_seconds = Some(age);
                issues.push("starting; no heartbeat yet".to_string());
            }
            Some(_) | None => issues.push("no heartbeat evidence".to_string()),
        }
    } else {
        issues.push("no bearer_state file".to_string());
    }

    let pid_path = if gist.is_empty() {
        home.join(format!("bearer_state.{channel}.pid"))
    } else {
        home.join(format!("bearer_gist.{}.pid", safe_gist(gist)))
    };
    let pid = read_pid(&pid_path);
    if pid == 0 {
        issues.push("no bearer pidfile".to_string());
    } else if !pid_alive(pid) {
        issues.push(format!("stale bearer pid {pid}"));
    }

    ChannelHealth {
        channel: channel.to_string(),
        status: if issues.is_empty() {
            HealthStatus::Ok
        } else {
            HealthStatus::Degraded
        },
        age_seconds,
        detail: if issues.is_empty() {
            if let Some(source) = signal_source.filter(|source| source != channel) {
                format!("fresh heartbeat via shared gist #{source}")
            } else {
                "fresh heartbeat".to_string()
            }
        } else {
            issues.join("; ")
        },
    }
}

fn load_config(path: &Path) -> ConfigFile {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| ConfigFile {
            subscribed_channels: Vec::new(),
            channel_gists: BTreeMap::new(),
        })
}

fn load_state(path: &Path) -> Option<BearerState> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn signal_timestamp(state: &BearerState) -> Option<f64> {
    state
        .last_heartbeat_ts
        .as_ref()
        .or(state.last_recv_ts.as_ref())
        .and_then(JsonNumber::as_f64)
}

fn signal_for_gist(
    home: &Path,
    gists: &BTreeMap<String, String>,
    channel: &str,
    gist: &str,
    own_state: Option<&BearerState>,
) -> Option<(f64, String)> {
    let mut best = own_state
        .and_then(signal_timestamp)
        .map(|ts| (ts, channel.to_string()));

    if gist.is_empty() {
        return best;
    }

    for (other, other_gist) in gists {
        if other == channel || other_gist != gist {
            continue;
        }
        let path = home.join(format!("bearer_state.{other}.json"));
        let Some(ts) = load_state(&path).as_ref().and_then(signal_timestamp) else {
            continue;
        };
        if best
            .as_ref()
            .map(|(best_ts, _)| ts > *best_ts)
            .unwrap_or(true)
        {
            best = Some((ts, other.clone()));
        }
    }

    best
}

fn file_age_seconds(path: &Path, now: f64) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let modified = modified.duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
    if now >= modified {
        Some((now - modified) as u64)
    } else {
        None
    }
}

fn read_pid(path: &Path) -> u32 {
    let Ok(raw) = fs::read_to_string(path) else {
        return 0;
    };
    raw.trim()
        .split_once('\t')
        .map(|(pid, _)| pid)
        .unwrap_or_else(|| raw.trim())
        .parse()
        .unwrap_or(0)
}

fn safe_gist(gist: &str) -> String {
    gist.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` does not send a signal; it asks the OS to
    // validate whether the process exists and is visible to this user.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    pid > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn safe_gist_replaces_non_alphanumeric_chars() {
        assert_eq!(safe_gist("abc-123/def"), "abc_123_def");
    }

    #[test]
    fn read_pid_uses_first_tab_separated_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        fs::write(&path, "123\tmetadata\n").unwrap();

        assert_eq!(read_pid(&path), 123);
    }

    #[test]
    fn fresh_heartbeat_and_live_pid_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join("config.json");
        let gist = "c68640ec0144b422c16b2d8c83ad5ee5";
        fs::write(
            &config,
            format!(
                r#"{{"subscribed_channels":["general"],"channel_gists":{{"general":"{gist}"}}}}"#
            ),
        )
        .unwrap();
        fs::write(
            home.join("bearer_state.general.json"),
            r#"{"last_heartbeat_ts":1000}"#,
        )
        .unwrap();
        fs::write(
            home.join(format!("bearer_gist.{}.pid", safe_gist(gist))),
            std::process::id().to_string(),
        )
        .unwrap();

        let rows = evaluate(home, &config, 90, 1000.0);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, HealthStatus::Ok);
    }

    #[test]
    fn stale_heartbeat_and_stale_pid_is_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join("config.json");
        let gist = "c68640ec0144b422c16b2d8c83ad5ee5";
        fs::write(
            &config,
            format!(
                r#"{{"subscribed_channels":["general"],"channel_gists":{{"general":"{gist}"}}}}"#
            ),
        )
        .unwrap();
        fs::write(
            home.join("bearer_state.general.json"),
            r#"{"last_heartbeat_ts":700}"#,
        )
        .unwrap();
        let mut pid_file =
            fs::File::create(home.join(format!("bearer_gist.{}.pid", safe_gist(gist)))).unwrap();
        writeln!(pid_file, "999999").unwrap();

        let rows = evaluate(home, &config, 90, 1000.0);

        assert_eq!(rows[0].status, HealthStatus::Degraded);
        assert!(rows[0].detail.contains("stale heartbeat 300s"));
        #[cfg(unix)]
        assert!(rows[0].detail.contains("stale bearer pid 999999"));
    }
}
