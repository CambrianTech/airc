use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io::{self, BufRead};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use super::rename::handle_rename;
use super::render::{
    client_attribute, normalize_channel, sanitize_channel, xml_escape, DropTracker, Sandbox,
};
use super::scope::Scope;
use crate::client_id::current_client_id;
use crate::legacy_envelope;
use crate::legacy_identity;
use crate::log_commands::{self, AppendOutcome};

const WATCHDOG_SEC: u64 = 150;
const ROTATE_EVERY_LINES: u64 = 100;
const DEFAULT_LOG_MAX_LINES: usize = 5_000;
const DEFAULT_LOG_KEEP_LINES: usize = 2_500;

pub(crate) struct Formatter {
    scope: Scope,
    room_name: String,
    client_id: Option<String>,
    seen_sigs: BTreeSet<String>,
    offset_counter: u64,
    sandbox: Sandbox,
    drops: DropTracker,
}

impl Formatter {
    pub(crate) fn new(scope: Scope) -> Self {
        let room_name = scope.room_name();
        let client_id = current_client_id().ok().flatten();
        let seen_sigs = load_seen_sigs(&scope.local_log, DEFAULT_LOG_MAX_LINES);
        let offset_counter = fs::read_to_string(&scope.offset_path)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0);
        Self {
            scope,
            room_name,
            client_id,
            seen_sigs,
            offset_counter,
            sandbox: Sandbox::new(),
            drops: DropTracker::new(),
        }
    }

    pub(crate) fn run_locked_stdin(&mut self) -> Result<(), Box<dyn Error>> {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            self.handle_line(&line);
        }
        Ok(())
    }

    pub(crate) fn run_with_watchdog(&mut self) -> Result<(), Box<dyn Error>> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });

        loop {
            match rx.recv_timeout(Duration::from_secs(WATCHDOG_SEC)) {
                Ok(line) => self.handle_line(&line),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    eprintln!("[airc:monitor] no inbound in {WATCHDOG_SEC}s - exiting for probe");
                    std::process::exit(2);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }

    fn handle_line(&mut self, raw: &str) {
        let line = raw.trim();
        if line.is_empty() {
            return;
        }
        self.offset_counter = self.offset_counter.saturating_add(1);
        let _ = fs::write(&self.scope.offset_path, self.offset_counter.to_string());

        let Ok(mut message) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if message.get("airc_heartbeat").and_then(Value::as_i64) == Some(1)
            || message.get("kind").and_then(Value::as_str) == Some("heartbeat")
        {
            return;
        }

        let from = string_field(&message, "from", "?");
        if legacy_envelope::is_encrypted(&message) {
            match self.decrypt_message(&mut message, &from) {
                Ok(()) => {}
                Err(error) => {
                    eprintln!("[airc:monitor] dropping encrypted msg from {from}: {error}");
                    return;
                }
            }
        }

        let mirrored_line = match serde_json::to_string(&message) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("[airc:formatter] skipped one line: {error}");
                return;
            }
        };
        if self.is_seen(&message) {
            return;
        }
        match log_commands::append_unique_sig(&self.scope.local_log, &mirrored_line) {
            Ok(AppendOutcome::Appended) => self.record_seen(&message),
            Ok(AppendOutcome::Skipped) => return,
            Err(_) => {}
        }

        if self.is_own_client_send(&message) {
            return;
        }
        if self.offset_counter.is_multiple_of(ROTATE_EVERY_LINES) {
            let _ = log_commands::run_rotate(
                &self.scope.local_log,
                env_usize("AIRC_LOG_MAX_LINES", DEFAULT_LOG_MAX_LINES),
                env_usize("AIRC_LOG_KEEP_LINES", DEFAULT_LOG_KEEP_LINES),
            );
        }

        let msg = message_text(&message);
        if handle_rename(&self.scope.peers_dir, &msg) {
            return;
        }
        if self.handle_ping_pong(&message, &from, &msg) {
            return;
        }
        self.display(&message, &from, &msg);
    }

    fn decrypt_message(&self, message: &mut Value, from: &str) -> Result<(), Box<dyn Error>> {
        let sender_pub = legacy_identity::peer_x25519_public_raw(&self.scope.peers_dir, from)?
            .ok_or("missing pubkey/privkey for decrypt")?;
        let my_priv = legacy_identity::load_x25519_private(&self.scope.identity_dir())
            .map_err(|_| "missing pubkey/privkey for decrypt")?;
        legacy_envelope::unwrap_value(message, my_priv, sender_pub)
            .map_err(|_| "unwrap failed (key mismatch, tampered, or malformed envelope)".into())
    }

    fn is_seen(&self, message: &Value) -> bool {
        sig(message).is_some_and(|sig| self.seen_sigs.contains(sig))
    }

    fn record_seen(&mut self, message: &Value) {
        if let Some(sig) = sig(message) {
            self.seen_sigs.insert(sig.to_string());
        }
    }

    fn is_own_client_send(&self, message: &Value) -> bool {
        self.client_id.as_deref().is_some_and(|client_id| {
            message.get("client_id").and_then(Value::as_str) == Some(client_id)
        })
    }

    fn handle_ping_pong(&self, message: &Value, from: &str, msg: &str) -> bool {
        if let Some(ping_id) = marker_uuid(msg, "PING") {
            let to = string_field(message, "to", "");
            if to == self.scope.current_name() {
                let mut command = Command::new("airc");
                command.arg("send").arg("--plaintext");
                let channel = string_field(message, "channel", "");
                if !channel.is_empty() {
                    command.arg("--channel").arg(channel);
                }
                let spawn = command
                    .arg(format!("@{from}"))
                    .arg(format!("[PONG:{ping_id}]"))
                    .stdout(Stdio::null())
                    .spawn();
                if let Err(error) = spawn {
                    eprintln!("[airc:monitor] auto-pong spawn failed: {error}");
                }
            }
            return true;
        }
        marker_uuid(msg, "PONG").is_some()
    }

    fn display(&mut self, message: &Value, from: &str, msg: &str) {
        let to = string_field(message, "to", "");
        let line_channel = string_field(message, "channel", &self.room_name);
        if let Some(subs) = self.scope.subscribed_channels() {
            let addressed_to_me = !to.is_empty()
                && to != "all"
                && to
                    .split(',')
                    .any(|target| target == self.scope.current_name());
            let line_norm = normalize_channel(&line_channel);
            let subs_norm = subs
                .iter()
                .map(|channel| normalize_channel(channel))
                .collect::<BTreeSet<_>>();
            if !matches!(from, "airc" | "sys")
                && !line_norm.is_empty()
                && !subs_norm.contains(&line_norm)
                && !addressed_to_me
            {
                self.drops.record(&line_norm, from);
                self.drops.maybe_emit_warning(&subs_norm);
                return;
            }
        }

        let safe_channel = sanitize_channel(&line_channel);
        let msg_one_line = msg.replace(['\n', '\r'], " ").trim().to_string();
        if matches!(from, "airc" | "sys") {
            println!("airc: [#{safe_channel}] {msg_one_line}");
            return;
        }

        self.sandbox.emit_contract_once();
        let client_attr = client_attribute(message);
        let to_attr = if to.is_empty() || to == "all" {
            String::new()
        } else {
            format!(" to=\"{}\"", xml_escape(&to))
        };
        println!(
            "airc: [#{safe_channel}] <pm-{} from=\"{}\"{} channel=\"{}\"{}>\n{}\n</pm-{}>",
            self.sandbox.nonce,
            xml_escape(from),
            client_attr,
            xml_escape(&line_channel),
            to_attr,
            xml_escape(&msg_one_line),
            self.sandbox.nonce
        );
    }
}

fn load_seen_sigs(path: &std::path::Path, limit: usize) -> BTreeSet<String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    raw.lines()
        .rev()
        .take(limit)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| {
            value
                .get("sig")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn sig(message: &Value) -> Option<&str> {
    message
        .get("sig")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn string_field(value: &Value, key: &str, default: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

fn message_text(value: &Value) -> String {
    value.get("msg").and_then(Value::as_str).map_or_else(
        || value.get("msg").map_or_else(String::new, Value::to_string),
        ToOwned::to_owned,
    )
}

fn marker_uuid<'a>(msg: &'a str, marker: &str) -> Option<&'a str> {
    let prefix = format!("[{marker}:");
    let rest = msg.strip_prefix(&prefix)?;
    let end = rest.find(']')?;
    let id = &rest[..end];
    Uuid::parse_str(id).ok()?;
    Some(id)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn own_client_send_detection_uses_client_id() {
        let dir = tempfile::tempdir().unwrap();
        let peers = dir.path().join("peers");
        fs::create_dir_all(&peers).unwrap();
        fs::write(
            dir.path().join("config.json"),
            r#"{"name":"alice","subscribed_channels":["general"]}"#,
        )
        .unwrap();
        let scope = Scope::new(&peers, "alice");
        let formatter = Formatter::new(scope);
        let message = json!({"from":"bob","client_id":"agent:someone-else"});

        assert!(!formatter.is_own_client_send(&message));
    }

    #[test]
    fn marker_uuid_requires_valid_uuid() {
        assert_eq!(
            marker_uuid("[PING:11111111-2222-3333-4444-555555555555]", "PING"),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(marker_uuid("[PING:not-a-uuid]", "PING"), None);
    }
}
