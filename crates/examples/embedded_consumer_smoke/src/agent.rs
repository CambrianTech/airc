//! Agent-consumer shape used by Codex/Claude-style runtimes.
//!
//! This is deliberately tiny product-adjacent code: it models what an
//! agent runtime needs from AIRC without importing substrate crates or
//! shelling out to the CLI.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use airc_lib::{
    Airc, AircError, Body, EventFilter, FilteredEventStream, HeaderFilter, Headers, PeerSpec,
    TranscriptCursor, TranscriptEvent, TranscriptKind,
};
use futures::StreamExt;

pub const HEADER_AGENT_KIND: &str = "airc.agent.kind";
pub const HEADER_AGENT_NAME: &str = "airc.agent.name";
pub const HEADER_AGENT_RUN_ID: &str = "airc.agent.run_id";

pub const AGENT_KIND_PROMPT: &str = "prompt";
pub const AGENT_KIND_STATUS: &str = "status";

#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub name: String,
    pub run_id: String,
}

impl AgentProfile {
    pub fn new(name: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            run_id: run_id.into(),
        }
    }
}

#[derive(Clone)]
pub struct AgentConsumer {
    airc: Airc,
    profile: AgentProfile,
}

impl AgentConsumer {
    pub async fn open(home: impl Into<PathBuf>, profile: AgentProfile) -> Result<Self, AircError> {
        Ok(Self {
            airc: Airc::open(home).await?,
            profile,
        })
    }

    pub fn airc(&self) -> &Airc {
        &self.airc
    }

    pub fn peer_spec(&self) -> String {
        self.airc.peer_spec()
    }

    pub async fn trust_peer_spec(&self, spec: &str) -> Result<(), AircError> {
        let peer: PeerSpec = spec.parse()?;
        self.airc.add_peer(peer).await
    }

    pub async fn join_shared_wire(
        &self,
        room: &str,
        wire: impl AsRef<Path>,
    ) -> Result<(), AircError> {
        self.airc
            .join_with_wire(room, wire.as_ref().to_path_buf())
            .await?;
        Ok(())
    }

    pub async fn send_prompt(&self, text: &str) -> Result<(), AircError> {
        self.send_agent_event(AGENT_KIND_PROMPT, text).await
    }

    pub async fn send_status(&self, text: &str) -> Result<(), AircError> {
        self.send_agent_event(AGENT_KIND_STATUS, text).await
    }

    pub async fn subscribe_prompts(&self) -> Result<AgentInbox, AircError> {
        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(TranscriptKind::Message);
        let filter = EventFilter {
            channel: None,
            channels: HashSet::new(),
            kinds,
            headers_filter: HeaderFilter::Exact {
                key: HEADER_AGENT_KIND.to_string(),
                value: AGENT_KIND_PROMPT.to_string(),
            },
        };
        Ok(AgentInbox {
            owner: self.clone(),
            stream: self.airc.subscribe_filtered(filter).await?,
        })
    }

    pub async fn resume_prompts_after(
        &self,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(TranscriptKind::Message);
        let filter = EventFilter {
            channel: None,
            channels: HashSet::new(),
            kinds,
            headers_filter: HeaderFilter::Exact {
                key: HEADER_AGENT_KIND.to_string(),
                value: AGENT_KIND_PROMPT.to_string(),
            },
        };
        Ok(self
            .airc
            .resume_from_filtered(cursor, filter, limit)
            .await?
            .into_iter()
            .filter(|event| !self.is_own_event(event))
            .collect())
    }

    fn is_own_event(&self, event: &TranscriptEvent) -> bool {
        event.peer_id == self.airc.peer_id() && event.client_id == self.airc.client_id()
    }

    async fn send_agent_event(&self, kind: &str, text: &str) -> Result<(), AircError> {
        let mut headers = Headers::new();
        headers.insert(HEADER_AGENT_KIND.to_string(), kind.to_string());
        headers.insert(HEADER_AGENT_NAME.to_string(), self.profile.name.clone());
        headers.insert(HEADER_AGENT_RUN_ID.to_string(), self.profile.run_id.clone());
        self.airc.send(Body::text(text), headers).await?;
        Ok(())
    }
}

pub struct AgentInbox {
    owner: AgentConsumer,
    stream: FilteredEventStream,
}

impl AgentInbox {
    pub async fn next_inbound(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<TranscriptEvent>, AircError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = deadline.saturating_duration_since(now);
            let event = tokio::time::timeout(remaining, self.stream.next()).await;
            match event {
                Ok(Some(Ok(event))) if self.owner.is_own_event(&event) => continue,
                Ok(Some(Ok(event))) => return Ok(Some(event.as_ref().clone())),
                Ok(Some(Err(lag))) => return Err(AircError::Route(lag.to_string())),
                Ok(None) => return Ok(None),
                Err(_) => return Ok(None),
            }
        }
    }
}
