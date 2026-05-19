use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::gh::{self};
use super::local_bus;
use super::outcome::SendKind;
use crate::gh_state::now_seconds;

const DEFAULT_POLL_INTERVAL: f64 = 15.0;
const DEFAULT_HEARTBEAT_INTERVAL: f64 = 30.0;
const SEEN_PAYLOAD_MAX: usize = 5000;

pub fn run_recv(
    peer_id: &str,
    _host_target: Option<&str>,
    _identity_key: Option<&str>,
    _remote_home: Option<&str>,
    offset_file: Option<&Path>,
    state_file: Option<&Path>,
    room_gist_id: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let Some(gist_id) = room_gist_id.filter(|gist| !gist.trim().is_empty()) else {
        return Err("bearer recv: no room_gist_id supplied".into());
    };
    let mut receiver = GistReceiver::new(peer_id, gist_id, offset_file, state_file);
    receiver.run()
}

struct GistReceiver<'a> {
    peer_id: &'a str,
    gist_id: &'a str,
    offset_file: Option<PathBuf>,
    state_file: Option<PathBuf>,
    consumed_lines: usize,
    local_byte_offset: u64,
    events_total: u64,
    seen: SeenPayloads,
    next_heartbeat_at: f64,
}

impl<'a> GistReceiver<'a> {
    fn new(
        peer_id: &'a str,
        gist_id: &'a str,
        offset_file: Option<&Path>,
        state_file: Option<&Path>,
    ) -> Self {
        let offset_file = offset_file.map(Path::to_path_buf);
        let state_file = state_file.map(Path::to_path_buf);
        let consumed_lines = read_usize_offset(offset_file.as_deref());
        let local_byte_offset =
            read_u64_offset(local_offset_file(offset_file.as_deref()).as_deref());
        Self {
            peer_id,
            gist_id,
            offset_file,
            state_file,
            consumed_lines,
            local_byte_offset,
            events_total: 0,
            seen: SeenPayloads::default(),
            next_heartbeat_at: now_seconds() + heartbeat_interval(),
        }
    }

    fn run(&mut self) -> Result<(), Box<dyn Error>> {
        self.write_state(json!({
            "kind": "gh",
            "peer_id": self.peer_id,
            "last_recv_ts": Value::Null,
            "last_sender": Value::Null,
            "events_total": 0,
            "diag": "bearer open, no events yet",
        }));

        loop {
            if !self.drain_local_bus()? {
                return Ok(());
            }
            match gh::get_classified(self.gist_id) {
                (Some(gist), _) => {
                    if !self.emit_new_gist_lines(&gist)? {
                        return Ok(());
                    }
                    self.sleep_or_heartbeat(poll_interval())?;
                }
                (None, SendKind::Gone) => {
                    return Err(format!("room gist {} returned 404 (gone)", self.gist_id).into());
                }
                (None, SendKind::SecondaryRateLimit) => {
                    self.sleep_or_heartbeat(poll_interval().max(60.0))?;
                }
                (None, _) => {
                    self.sleep_or_heartbeat(poll_interval())?;
                }
            }
        }
    }

    fn drain_local_bus(&mut self) -> Result<bool, Box<dyn Error>> {
        let (lines, new_offset) = local_bus::read_from(self.gist_id, self.local_byte_offset);
        if new_offset != self.local_byte_offset {
            self.local_byte_offset = new_offset;
            write_offset(
                local_offset_file(self.offset_file.as_deref()).as_deref(),
                new_offset,
            );
        }
        for line in lines {
            if !self.emit_line(&line, "local bus")? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn emit_new_gist_lines(&mut self, gist: &Value) -> Result<bool, Box<dyn Error>> {
        let content = gh::read_messages_content(gist);
        let lines = content.lines().collect::<Vec<_>>();
        if self.consumed_lines > lines.len() {
            self.consumed_lines = lines.len();
            write_offset(self.offset_file.as_deref(), self.consumed_lines);
        }
        for (idx, line) in lines.iter().enumerate().skip(self.consumed_lines) {
            self.consumed_lines = idx + 1;
            write_offset(self.offset_file.as_deref(), self.consumed_lines);
            if !self.emit_line(line, "gh poll")? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn emit_line(&mut self, line: &str, source: &str) -> Result<bool, Box<dyn Error>> {
        let Some((sender, _channel)) = parse_legacy_envelope(line) else {
            return Ok(true);
        };
        if self.seen.check(line) {
            return Ok(true);
        }
        let mut stdout = io::stdout().lock();
        if writeln!(stdout, "{line}")
            .and_then(|_| stdout.flush())
            .is_err()
        {
            return Ok(false);
        }
        self.events_total += 1;
        self.write_state(json!({
            "kind": "gh",
            "peer_id": self.peer_id,
            "last_recv_ts": now_seconds(),
            "last_sender": sender,
            "events_total": self.events_total,
            "diag": format!("last event from {source}"),
            "last_heartbeat_ts": now_seconds(),
        }));
        Ok(true)
    }

    fn sleep_or_heartbeat(&mut self, seconds: f64) -> Result<(), Box<dyn Error>> {
        let deadline = now_seconds() + seconds.max(0.0);
        while now_seconds() < deadline {
            self.maybe_emit_heartbeat()?;
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn maybe_emit_heartbeat(&mut self) -> Result<(), Box<dyn Error>> {
        let now = now_seconds();
        if now < self.next_heartbeat_at {
            return Ok(());
        }
        let line = json!({
            "airc_heartbeat": 1,
            "ts": now,
            "channel": self.gist_id,
        })
        .to_string();
        let mut stdout = io::stdout().lock();
        if writeln!(stdout, "{line}")
            .and_then(|_| stdout.flush())
            .is_err()
        {
            return Err("stdout closed".into());
        }
        self.touch_state_heartbeat(now);
        self.next_heartbeat_at = now + heartbeat_interval();
        Ok(())
    }

    fn write_state(&self, state: Value) {
        let Some(path) = &self.state_file else {
            return;
        };
        write_json_atomic(path, &state);
    }

    fn touch_state_heartbeat(&self, ts: f64) {
        let Some(path) = &self.state_file else {
            return;
        };
        let mut state = fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| json!({}));
        state["last_heartbeat_ts"] = json!(ts);
        write_json_atomic(path, &state);
    }
}

#[derive(Default)]
struct SeenPayloads {
    values: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenPayloads {
    fn check(&mut self, line: &str) -> bool {
        if self.values.contains(line) {
            return true;
        }
        let owned = line.to_string();
        self.values.insert(owned.clone());
        self.order.push_back(owned);
        while self.order.len() > SEEN_PAYLOAD_MAX {
            if let Some(old) = self.order.pop_front() {
                self.values.remove(&old);
            }
        }
        false
    }
}

fn parse_legacy_envelope(line: &str) -> Option<(String, String)> {
    let env = serde_json::from_str::<Value>(line).ok()?;
    let sender = env.get("from")?.as_str()?.to_string();
    let channel = env
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some((sender, channel))
}

fn local_offset_file(offset_file: Option<&Path>) -> Option<PathBuf> {
    offset_file.map(|path| PathBuf::from(format!("{}.local.bytes", path.display())))
}

fn read_usize_offset(path: Option<&Path>) -> usize {
    let Some(path) = path else {
        return 0;
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

fn read_u64_offset(path: Option<&Path>) -> u64 {
    let Some(path) = path else {
        return 0;
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn write_offset<T: std::fmt::Display>(path: Option<&Path>, value: T) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, value.to_string());
}

fn write_json_atomic(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if fs::write(&tmp, value.to_string()).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

fn poll_interval() -> f64 {
    env_f64("AIRC_GH_POLL_INTERVAL", DEFAULT_POLL_INTERVAL)
}

fn heartbeat_interval() -> f64 {
    env_f64("AIRC_BEARER_HEARTBEAT_SEC", DEFAULT_HEARTBEAT_INTERVAL)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|value| *value >= 0.0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_legacy_envelope_requires_sender() {
        assert_eq!(
            parse_legacy_envelope(r#"{"from":"alice","channel":"general"}"#),
            Some(("alice".to_string(), "general".to_string()))
        );
        assert_eq!(parse_legacy_envelope(r#"{"channel":"general"}"#), None);
        assert_eq!(parse_legacy_envelope("not json"), None);
    }

    #[test]
    fn seen_payloads_dedupes_exact_lines() {
        let mut seen = SeenPayloads::default();
        assert!(!seen.check("a"));
        assert!(seen.check("a"));
        assert!(!seen.check("b"));
    }
}
