//! Runtime client identity helpers for shell integration.

use std::env;
use std::error::Error;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::process::Command;

#[cfg(any(unix, test))]
use sha2::{Digest, Sha256};

#[cfg(any(unix, test))]
use airc_core::humanhash;
use airc_core::{ClientId, TranscriptEvent};
use airc_protocol::HEADER_AIRC_CLIENT;

/// Self-echo policy for one runtime sharing a durable AIRC identity.
/// Borrows the runtime tag; filtering inspects only headers and the client ID.
pub(crate) struct RuntimeSelfFilter<'a> {
    client_id: ClientId,
    runtime_client: Option<&'a str>,
}

impl<'a> RuntimeSelfFilter<'a> {
    pub(crate) fn new(client_id: ClientId, runtime_client: Option<&'a str>) -> Self {
        Self {
            client_id,
            runtime_client,
        }
    }

    pub(crate) fn is_self_event(&self, event: &TranscriptEvent) -> bool {
        // Shared HOME can share BOTH peer_id and client_id. A stamped runtime
        // header is authoritative: a different tag (or no local tag) stays
        // visible, regardless of either durable identity matching.
        if let Some(event_client) = event.headers.get(HEADER_AIRC_CLIENT) {
            return self.runtime_client.is_some_and(|rc| event_client == rc);
        }
        // Historical unstamped events retain the Codex hook's narrower client
        // fallback. It is still ambiguous on shared HOME; peer equality cannot
        // resolve that ambiguity and must never hide another runtime's header.
        event.client_id == self.client_id
    }
}

#[cfg(any(unix, test))]
const AGENT_PREFIX: &str = "agent:";
const CODEX_PREFIX: &str = "codex:";
const CLAUDE_PREFIX: &str = "claude:";
#[cfg(unix)]
const PROCESS_WALK_LIMIT: usize = 16;

pub fn current_client_id() -> Result<Option<String>, Box<dyn Error>> {
    if let Some(value) = non_empty_env("AIRC_CLIENT_ID") {
        return Ok(Some(value));
    }
    if let Some(value) = non_empty_env("CODEX_THREAD_ID") {
        return Ok(Some(format!("{CODEX_PREFIX}{value}")));
    }
    if let Some(value) = non_empty_env("CLAUDE_CODE_SESSION_ID") {
        return Ok(Some(format!("{CLAUDE_PREFIX}{value}")));
    }
    if let Some(value) = non_empty_env("CLAUDE_SESSION_ID") {
        return Ok(Some(format!("{CLAUDE_PREFIX}{value}")));
    }

    agent_process_client_id()
}

#[cfg(any(unix, test))]
pub fn agent_label(seed: &str) -> Result<String, Box<dyn Error>> {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let hex = to_hex(&digest);
    Ok(format!("{AGENT_PREFIX}{}", humanhash(&hex, 4)?))
}

#[cfg(unix)]
fn agent_process_client_id() -> Result<Option<String>, Box<dyn Error>> {
    let mut pid = std::process::id();
    for _ in 0..PROCESS_WALK_LIMIT {
        let Some(process) = read_process(pid)? else {
            return Ok(None);
        };
        if is_agent_command(&process.command) {
            return Ok(Some(agent_label(&format!("{pid}:{}", process.command))?));
        }
        if process.parent_pid <= 1 {
            return Ok(None);
        }
        pid = process.parent_pid;
    }
    Ok(None)
}

#[cfg(not(unix))]
fn agent_process_client_id() -> Result<Option<String>, Box<dyn Error>> {
    Ok(None)
}

#[cfg(unix)]
struct ProcessRow {
    parent_pid: u32,
    command: String,
}

#[cfg(unix)]
fn read_process(pid: u32) -> Result<Option<ProcessRow>, Box<dyn Error>> {
    let output = match Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,command="])
        .output()
    {
        Ok(output) => output,
        // `ps` is not installed (slim containers ship without procps).
        // Client-id detection is BEST-EFFORT — it tags a frame with the
        // calling agent when discoverable, nothing more. A missing probe
        // must degrade to "can't detect" (Ok(None)), NEVER propagate os
        // error 2 up through runtime_headers() and break EVERY send/msg
        // (the bug that left containerized nodes able to converge but
        // unable to deliver a single frame). Genuinely unexpected `ps`
        // failures still surface.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !output.status.success() {
        return Ok(None);
    }

    let text = String::from_utf8(output.stdout)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let parent_pid = parts
        .next()
        .ok_or("missing parent pid")?
        .trim()
        .parse::<u32>()?;
    let command = parts.next().unwrap_or("").trim().to_string();
    Ok(Some(ProcessRow {
        parent_pid,
        command,
    }))
}

#[cfg(unix)]
fn is_agent_command(command: &str) -> bool {
    let argv0 = command.split_whitespace().next().unwrap_or("");
    let base = Path::new(argv0)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(argv0);
    matches!(base, "claude" | "codex") || command.contains("/codex/codex")
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

#[cfg(any(unix, test))]
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use airc_core::{
        Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptKind,
    };

    use super::{
        agent_label, current_client_id, RuntimeSelfFilter, TranscriptEvent, HEADER_AIRC_CLIENT,
    };

    #[test]
    fn runtime_self_filter_header_is_authoritative_across_shared_identities() {
        let local_client = ClientId::new();
        let local_peer = PeerId::new();
        let self_filter = RuntimeSelfFilter::new(local_client, Some("codex:thread-1"));
        let no_runtime = RuntimeSelfFilter::new(local_client, None);
        let mut event = TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: local_peer,
            client_id: local_client,
            kind: TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: 1,
            target: MentionTarget::All,
            headers: Headers::new(),
            // Identity-like body content is deliberately irrelevant. The
            // predicate must not sniff or deserialize it to determine self.
            body: Some(Body::text(r#"{"airc.client":"codex:thread-1"}"#)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };

        for peer_id in [local_peer, PeerId::new()] {
            for client_id in [local_client, ClientId::new()] {
                event.peer_id = peer_id;
                event.client_id = client_id;
                event
                    .headers
                    .insert(HEADER_AIRC_CLIENT.to_string(), "codex:thread-1".to_string());
                assert!(self_filter.is_self_event(&event));
                assert!(!no_runtime.is_self_event(&event));

                event.headers.insert(
                    HEADER_AIRC_CLIENT.to_string(),
                    "claude:session-1".to_string(),
                );
                assert!(!self_filter.is_self_event(&event));
                assert!(!no_runtime.is_self_event(&event));

                // An unrelated header cannot impersonate the runtime tag;
                // unstamped events keep the existing durable-client fallback.
                event.headers.remove(HEADER_AIRC_CLIENT);
                event
                    .headers
                    .insert("other.client".to_string(), "codex:thread-1".to_string());
                assert_eq!(self_filter.is_self_event(&event), client_id == local_client);
                assert_eq!(no_runtime.is_self_event(&event), client_id == local_client);
            }
        }
    }

    #[test]
    fn explicit_env_wins() {
        temp_env::with_var("AIRC_CLIENT_ID", Some("explicit"), || {
            temp_env::with_var("CODEX_THREAD_ID", Some("ignored"), || {
                assert_eq!(current_client_id().unwrap(), Some("explicit".to_string()));
            });
        });
    }

    #[test]
    fn codex_thread_id_is_namespaced() {
        temp_env::with_var("AIRC_CLIENT_ID", None::<&str>, || {
            temp_env::with_var("CODEX_THREAD_ID", Some("thread-1"), || {
                assert_eq!(
                    current_client_id().unwrap(),
                    Some("codex:thread-1".to_string())
                );
            });
        });
    }

    // what this catches: client-id detection is best-effort — when `ps`
    // is absent (slim containers ship without procps) current_client_id()
    // must return Ok(None), NOT propagate os error 2. That ENOENT
    // propagated through runtime_headers() and broke EVERY send/msg —
    // containerized nodes converged but could not deliver a single frame.
    // Empty PATH makes `ps` unresolvable on any runner, so this is
    // deterministic regardless of whether the host has procps.
    #[cfg(unix)]
    #[test]
    fn missing_ps_degrades_to_ok_not_os_error_2() {
        temp_env::with_vars(
            [
                ("AIRC_CLIENT_ID", None::<&str>),
                ("CODEX_THREAD_ID", None),
                ("CLAUDE_CODE_SESSION_ID", None),
                ("CLAUDE_SESSION_ID", None),
                ("PATH", Some("")),
            ],
            || {
                assert!(
                    current_client_id().is_ok(),
                    "ps-absent must degrade to Ok, never break the send path with os error 2"
                );
            },
        );
    }

    #[test]
    fn agent_label_is_mnemonic_not_raw_seed() {
        let label = agent_label("300:/Users/example/.local/bin/claude --resume").unwrap();

        assert!(label.starts_with("agent:"));
        assert_eq!(label.split('-').count(), 4);
        assert!(!label.contains("300"));
    }
}
