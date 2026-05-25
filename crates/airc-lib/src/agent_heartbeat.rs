//! Active-agent heartbeat events.
//!
//! Closes GRID-SUBSTRATE-AUDIT flaw #6 from #964: "the active-agent
//! loop is not yet self-managing." Before this module, observers had
//! to read inbox prose ("claude-tab-1 back online") to know which
//! agents were alive. After this module, agents emit typed
//! heartbeats on an interval and any observer can query
//! [`Airc::active_agents(within)`] to get a current liveness view.
//!
//! Headers (filterable without body decode):
//! - `airc.heartbeat.kind` = `"alive"` (reserved space for future
//!   kinds: `"leaving"`, `"degraded"`).
//! - `airc.heartbeat.runtime` = caller-supplied runtime label
//!   (e.g. `"claude"`, `"codex"`, `"interactive"`).
//! - `airc.heartbeat.client` = optional runtime client id
//!   (`"codex:..."`, `"claude:..."`) when known.
//!
//! Frame kind is `Event` (durable — same trade-off the audit's flaw
//! #4 flags). 60s default emit interval keeps noise bounded. The
//! ephemeral-vs-durable substrate split is the right long-term fix
//! and is its own follow-up.
//!
//! Scope cut: this module ships the typed event + emit task + query.
//! It does **not** ship:
//! - Automatic stop when the process exits (caller owns the handle's
//!   lifecycle; dropping aborts the task).
//! - Cross-room scoping (heartbeats go to the current default room).
//! - A `"leaving"` event on graceful shutdown (caller can emit one
//!   manually via [`Airc::emit_agent_heartbeat`] with a custom kind
//!   if needed; the typed shape is reserved for it).

use std::sync::Arc;
use std::time::Duration;

use airc_core::headers::Headers;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, PeerId, TranscriptEvent};
use airc_protocol::FrameKind;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

pub const HEADER_HEARTBEAT_KIND: &str = "airc.heartbeat.kind";
pub const HEADER_HEARTBEAT_RUNTIME: &str = "airc.heartbeat.runtime";
pub const HEADER_HEARTBEAT_CLIENT: &str = "airc.heartbeat.client";

/// Default 60s emit cadence. Tuned so a peer that hasn't beat in
/// 3× the default is unambiguously stale, while keeping heartbeat
/// noise low on a busy inbox.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// What the heartbeat is asserting. Reserved space for future kinds
/// (`Leaving`, `Degraded`) so observers can switch on this string
/// rather than parsing free-text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatKind {
    /// Periodic "I'm still here" beat.
    Alive,
    /// Caller is shutting down gracefully — observers should mark
    /// this peer offline immediately, not wait for staleness.
    Leaving,
}

impl HeartbeatKind {
    pub fn header_value(self) -> &'static str {
        match self {
            HeartbeatKind::Alive => "alive",
            HeartbeatKind::Leaving => "leaving",
        }
    }
}

/// One typed heartbeat record. Body of the durable event; substrate
/// headers `airc.heartbeat.kind` / `runtime` carry the same values
/// so observers can filter without decoding the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    pub kind: HeartbeatKind,
    pub peer: PeerId,
    /// Caller-supplied runtime label. Convention: lowercase identifier
    /// (`"claude"`, `"codex"`, `"interactive"`, `"automation"`).
    pub runtime: String,
    /// Optional runtime-client id. This distinguishes multiple tabs
    /// sharing the same durable peer identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Optional caller-supplied scope label. Useful when one agent
    /// runs from multiple project worktrees and observers want to
    /// distinguish them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Optional build identifier for operators checking whether idle
    /// or broken agents are on stale code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub emitted_at_ms: u64,
}

/// Summary of one currently-alive agent. Returned by
/// [`Airc::active_agents`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLiveness {
    pub peer: PeerId,
    pub runtime: String,
    pub client_id: Option<String>,
    pub scope: Option<String>,
    pub build: Option<String>,
    pub last_seen_ms: u64,
}

/// Handle to a running heartbeat emit task. Dropping or calling
/// [`HeartbeatTask::stop`] aborts the task.
pub struct HeartbeatTask {
    inner: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl HeartbeatTask {
    fn new(handle: JoinHandle<()>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(handle))),
        }
    }

    /// Stop the heartbeat task. Aborts the spawned tokio task; no
    /// `Leaving` event is emitted. Callers that want a graceful
    /// "I'm leaving" signal should call
    /// [`Airc::emit_agent_heartbeat`] with [`HeartbeatKind::Leaving`]
    /// before dropping the handle.
    pub async fn stop(self) {
        if let Some(handle) = self.inner.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl Drop for HeartbeatTask {
    fn drop(&mut self) {
        let inner = self.inner.clone();
        // Best-effort abort on drop. We can't await from Drop, so
        // spawn a tiny task that takes the handle and aborts.
        // Skipped if a runtime isn't available (unusual outside of
        // tests).
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                if let Some(handle) = inner.lock().await.take() {
                    handle.abort();
                }
            });
        }
    }
}

impl Airc {
    /// Emit a single heartbeat. Useful for ad-hoc beats (e.g.
    /// `Leaving` on graceful shutdown) outside the periodic task.
    pub async fn emit_agent_heartbeat(
        &self,
        kind: HeartbeatKind,
        runtime: impl Into<String>,
        scope: Option<String>,
    ) -> Result<(), AircError> {
        self.emit_agent_heartbeat_with_metadata(kind, runtime, None, scope, None)
            .await
    }

    /// Emit a heartbeat with runtime metadata. CLI `join` uses this
    /// form so managers can distinguish multiple tabs sharing one
    /// peer identity and spot stale builds.
    pub async fn emit_agent_heartbeat_with_metadata(
        &self,
        kind: HeartbeatKind,
        runtime: impl Into<String>,
        client_id: Option<String>,
        scope: Option<String>,
        build: Option<String>,
    ) -> Result<(), AircError> {
        let runtime = runtime.into();
        let heartbeat = AgentHeartbeat {
            kind,
            peer: self.peer_id(),
            runtime: runtime.clone(),
            client_id: client_id.clone(),
            scope,
            build,
            emitted_at_ms: now_ms()?,
        };
        let body = serde_json::to_value(&heartbeat)
            .map_err(|error| AircError::Crypto(format!("agent heartbeat encode: {error}")))?;
        let mut headers = Headers::new();
        headers.insert(
            HEADER_HEARTBEAT_KIND.into(),
            kind.header_value().to_string(),
        );
        headers.insert(HEADER_HEARTBEAT_RUNTIME.into(), runtime);
        if let Some(client_id) = client_id {
            headers.insert(HEADER_HEARTBEAT_CLIENT.into(), client_id);
        }
        self.send_frame_to(
            FrameKind::Event,
            MentionTarget::All,
            Body::Json(body),
            headers,
        )
        .await?;
        Ok(())
    }

    /// Spawn a background task that emits `Alive` heartbeats every
    /// `interval`. Returns a handle that aborts the task when
    /// dropped or [`HeartbeatTask::stop`]ed.
    ///
    /// Pass [`DEFAULT_HEARTBEAT_INTERVAL`] for the standard 60s
    /// cadence. Shorter intervals are useful in tests; longer
    /// intervals are useful for low-traffic agents that don't need
    /// fine-grained liveness.
    pub async fn start_agent_heartbeat(
        &self,
        runtime: impl Into<String>,
        scope: Option<String>,
        interval: Duration,
    ) -> Result<HeartbeatTask, AircError> {
        self.start_agent_heartbeat_with_metadata(runtime, None, scope, None, interval)
            .await
    }

    /// Spawn a heartbeat task with runtime metadata.
    pub async fn start_agent_heartbeat_with_metadata(
        &self,
        runtime: impl Into<String>,
        client_id: Option<String>,
        scope: Option<String>,
        build: Option<String>,
        interval: Duration,
    ) -> Result<HeartbeatTask, AircError> {
        let runtime = runtime.into();
        // Emit one beat synchronously so observers see the agent
        // alive immediately, before the first interval tick.
        self.emit_agent_heartbeat_with_metadata(
            HeartbeatKind::Alive,
            runtime.clone(),
            client_id.clone(),
            scope.clone(),
            build.clone(),
        )
        .await?;

        let airc = self.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first tick since we already emitted above.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Err(error) = airc
                    .emit_agent_heartbeat_with_metadata(
                        HeartbeatKind::Alive,
                        runtime.clone(),
                        client_id.clone(),
                        scope.clone(),
                        build.clone(),
                    )
                    .await
                {
                    eprintln!("agent heartbeat emit failed: {error}");
                }
            }
        });
        Ok(HeartbeatTask::new(handle))
    }

    /// Return the agents whose most recent heartbeat falls within
    /// `within` of the current wall-clock time. Walks the recent
    /// transcript page (`window` events) and reduces to one entry
    /// per peer (latest-wins).
    ///
    /// Excludes peers whose latest heartbeat was [`HeartbeatKind::
    /// Leaving`] — that's a deliberate offline signal.
    pub async fn active_agents(
        &self,
        within: Duration,
        window: usize,
    ) -> Result<Vec<AgentLiveness>, AircError> {
        let now = now_ms()?;
        let cutoff = now.saturating_sub(within.as_millis() as u64);
        let recent = self.page_recent(window).await?;
        let mut latest: std::collections::HashMap<AgentHeartbeatKey, AgentHeartbeat> =
            std::collections::HashMap::new();
        for event in &recent {
            let Some(beat) = parse_heartbeat(event) else {
                continue;
            };
            if beat.emitted_at_ms < cutoff {
                continue;
            }
            // Keep the highest emitted_at_ms per peer.
            latest
                .entry(AgentHeartbeatKey::from(&beat))
                .and_modify(|existing| {
                    if beat.emitted_at_ms > existing.emitted_at_ms {
                        *existing = beat.clone();
                    }
                })
                .or_insert(beat);
        }
        let mut alive: Vec<AgentLiveness> = latest
            .into_values()
            .filter(|beat| beat.kind == HeartbeatKind::Alive)
            .map(|beat| AgentLiveness {
                peer: beat.peer,
                runtime: beat.runtime,
                client_id: beat.client_id,
                scope: beat.scope,
                build: beat.build,
                last_seen_ms: beat.emitted_at_ms,
            })
            .collect();
        alive.sort_by_key(|liveness| {
            format!(
                "{}:{}",
                liveness.peer,
                liveness.client_id.as_deref().unwrap_or("")
            )
        });
        Ok(alive)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AgentHeartbeatKey {
    peer: PeerId,
    client_id: Option<String>,
}

impl AgentHeartbeatKey {
    fn from(beat: &AgentHeartbeat) -> Self {
        Self {
            peer: beat.peer,
            client_id: beat.client_id.clone(),
        }
    }
}

fn parse_heartbeat(event: &TranscriptEvent) -> Option<AgentHeartbeat> {
    let _ = event.headers.get(HEADER_HEARTBEAT_KIND)?;
    let body = event.body.as_ref()?;
    let value = match body {
        Body::Json(value) => value.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_alive() -> AgentHeartbeat {
        AgentHeartbeat {
            kind: HeartbeatKind::Alive,
            peer: PeerId::new(),
            runtime: "claude".to_string(),
            client_id: Some("claude:tab-1".to_string()),
            scope: Some("/work/airc".to_string()),
            build: Some("abc123".to_string()),
            emitted_at_ms: 1_700_000_000_000,
        }
    }

    fn sample_leaving() -> AgentHeartbeat {
        AgentHeartbeat {
            kind: HeartbeatKind::Leaving,
            peer: PeerId::new(),
            runtime: "codex".to_string(),
            client_id: None,
            scope: None,
            build: None,
            emitted_at_ms: 1_700_000_001_000,
        }
    }

    #[test]
    fn alive_heartbeat_round_trips_through_json() {
        let beat = sample_alive();
        let json = serde_json::to_string(&beat).expect("encode");
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, beat);
    }

    #[test]
    fn leaving_heartbeat_round_trips_through_json() {
        let beat = sample_leaving();
        let json = serde_json::to_string(&beat).expect("encode");
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, beat);
    }

    #[test]
    fn header_values_stable() {
        assert_eq!(HeartbeatKind::Alive.header_value(), "alive");
        assert_eq!(HeartbeatKind::Leaving.header_value(), "leaving");
    }

    #[test]
    fn scope_is_optional_in_json() {
        let beat = AgentHeartbeat {
            kind: HeartbeatKind::Alive,
            peer: PeerId::new(),
            runtime: "interactive".to_string(),
            client_id: None,
            scope: None,
            build: None,
            emitted_at_ms: 0,
        };
        let json = serde_json::to_string(&beat).expect("encode");
        // `serde(skip_serializing_if = "Option::is_none")` should
        // omit the scope key entirely when None.
        assert!(!json.contains("\"scope\""));
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.scope, None);
    }
}
