//! airc as an ACP **client** — one bridge, N agents.
//!
//! See `docs/architecture/ACP-CLIENT-BRIDGE.md` for why this shape rather than a
//! per-agent plugin. Short version: Hermes already ships an
//! `agent-client-protocol` adapter over JSON-RPC stdio, as do the other entries
//! in the ACP registry. Writing a Hermes-specific plugin would buy one agent and
//! couple us to their internals; speaking the standard instead makes every ACP
//! agent a room citizen with zero upstream patches.
//!
//! ## Shape
//!
//! ```text
//!   room message  ──▶  PromptRequest         ──▶  agent subprocess (stdio JSON-RPC)
//!   room message  ◀──  SessionNotification   ◀──
//!                      RequestPermissionRequest ─▶ ToolPolicy (deny by default)
//! ```
//!
//! The agent is a child process the bridge owns. `AcpAgent::from_str("hermes-acp")`
//! spawns it and serves as the transport.

pub mod policy;
pub mod transport;

pub use policy::{PeerPolicy, PermissionDecision, ToolIdentity, ToolPolicy};
pub use transport::AcpBridge;

use std::fmt;
use std::path::PathBuf;

/// How to launch one ACP agent, and what it is allowed to do once running.
///
/// The command is a plain string (`"hermes-acp"`, `"uvx hermes-agent[acp]"`,
/// `"python my_agent.py"`) because that is what the ACP registry publishes and
/// what the SDK's `AcpAgent::from_str` consumes. Keeping it a string means a new
/// agent is a config line, not a code change — which is the entire point of
/// speaking the standard.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    /// Command line that launches the agent in ACP mode.
    pub command: String,
    /// Which tools it may run on behalf of the room. Defaults to nothing.
    pub tools: ToolPolicy,
    /// Which peers may prompt it. Defaults to anyone in the room.
    pub peers: PeerPolicy,
    /// The directory the agent treats as its workspace.
    ///
    /// `None` means the bridge process's current directory. Named explicitly
    /// rather than always-implicit because the bridge may run as a daemon whose
    /// cwd is unrelated to any project — an agent silently rooted at `/` is a
    /// confusing way to find out.
    pub workspace: Option<PathBuf>,
}

impl AgentSpec {
    /// A spec that can think but cannot act: it will answer prompts from anyone
    /// in the room and refuse every tool.
    ///
    /// This is the honest default for an agent nobody has configured yet. It is
    /// useful immediately (chat works) and cannot be turned into a foothold by a
    /// peer, which is the property that matters when the prompter is arbitrary.
    pub fn talk_only(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            tools: ToolPolicy::deny_all(),
            peers: PeerPolicy::any(),
            workspace: None,
        }
    }

    pub fn with_tools(mut self, tools: ToolPolicy) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_peers(mut self, peers: PeerPolicy) -> Self {
        self.peers = peers;
        self
    }

    pub fn with_workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }
}

/// Why a bridge attempt failed, in terms an operator can act on.
///
/// Deliberately NOT a catch-all string: "the agent did not answer" and "the
/// agent refused the peer" lead to different fixes, and a bridge that collapses
/// them sends the reader to the wrong layer — the failure mode this whole
/// session kept running into.
#[derive(Debug)]
pub enum BridgeError {
    /// The peer is not permitted to prompt this agent.
    PeerRefused { peer_id: String },
    /// The agent process could not be spawned or did not speak ACP.
    AgentUnavailable { command: String, detail: String },
    /// The agent was reached but the exchange failed.
    Exchange { detail: String },
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BridgeError::PeerRefused { peer_id } => write!(
                f,
                "peer {peer_id} is not in this agent's allowFrom list — it may read the room but not prompt this agent"
            ),
            BridgeError::AgentUnavailable { command, detail } => write!(
                f,
                "could not start ACP agent `{command}`: {detail} — check it is installed and supports ACP (e.g. `uvx hermes-agent[acp]`)"
            ),
            BridgeError::Exchange { detail } => {
                write!(f, "ACP exchange failed: {detail}")
            }
        }
    }
}

impl std::error::Error for BridgeError {}

/// One turn's worth of result, ready to publish back into the room.
#[derive(Debug, Clone, Default)]
pub struct AgentTurn {
    /// The agent's reply text, assembled from its streamed updates.
    pub text: String,
    /// Every permission decision made during the turn, in order.
    ///
    /// Carried out of the turn rather than logged and forgotten: a refusal that
    /// only reaches a log file is invisible to the room, and the room is where
    /// the person wondering why the agent stopped short is looking.
    pub decisions: Vec<PermissionDecision>,
}

impl AgentTurn {
    /// Did anything get refused this turn?
    pub fn had_refusal(&self) -> bool {
        self.decisions.iter().any(|d| !d.is_allowed())
    }

    /// The lines to publish into the room alongside the reply, or empty when the
    /// turn needed no permissions. Refusals are surfaced; approvals are not —
    /// announcing every permitted tool would flood the room and train readers to
    /// ignore the line that matters.
    pub fn refusal_lines(&self) -> Vec<String> {
        self.decisions
            .iter()
            .filter(|d| !d.is_allowed())
            .map(|d| d.to_room_line())
            .collect()
    }
}

/// Decide whether `peer_id` may prompt `spec`, before any process is spawned.
///
/// Split out and pure so the gate is testable without an agent, and so the
/// refusal happens BEFORE we pay for a subprocess — a peer who cannot prompt
/// should not be able to make us fork one.
pub fn admit_prompt(spec: &AgentSpec, peer_id: &str) -> Result<(), BridgeError> {
    if spec.peers.may_prompt(peer_id) {
        Ok(())
    } else {
        Err(BridgeError::PeerRefused {
            peer_id: peer_id.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches: a default that can act. An unconfigured agent must be
    /// able to CHAT (that is the point of the bridge) while refusing every tool —
    /// in a room the prompter is an arbitrary peer.
    #[test]
    fn the_default_spec_can_think_but_not_act() {
        let spec = AgentSpec::talk_only("hermes-acp");
        assert!(
            spec.peers.may_prompt("peer-anyone"),
            "chat must work by default"
        );
        let execute = ToolIdentity {
            kind: Some("execute".into()),
            title: Some("run a shell command".into()),
        };
        assert!(!spec.tools.decide(&execute).is_allowed(), "tools must not");
    }

    /// what this catches: spawning a subprocess for a peer we were going to
    /// refuse anyway. The gate must be decidable before any process exists.
    #[test]
    fn a_refused_peer_is_rejected_before_any_spawn() {
        let spec = AgentSpec::talk_only("hermes-acp").with_peers(PeerPolicy::only(["peer-a"]));
        let err = admit_prompt(&spec, "peer-b").unwrap_err();
        assert!(matches!(err, BridgeError::PeerRefused { .. }));
        // The message must name the distinction, or the operator reads it as "the
        // agent is broken" rather than "the agent is configured".
        assert!(err.to_string().contains("allowFrom"), "{err}");
        assert!(admit_prompt(&spec, "peer-a").is_ok());
    }

    /// what this catches: refusals dying in a log. The room is where someone is
    /// wondering why the agent stopped short, so the turn must carry them out.
    #[test]
    fn refusals_travel_with_the_turn_and_approvals_stay_quiet() {
        let turn = AgentTurn {
            text: "I could not write that file.".into(),
            decisions: vec![
                PermissionDecision::Allow {
                    label: "read src/main.rs (read)".into(),
                    caveat: None,
                },
                PermissionDecision::Deny {
                    label: "write src/main.rs (edit)".into(),
                    reason: "not allow-listed".into(),
                },
            ],
        };
        assert!(turn.had_refusal());
        let lines = turn.refusal_lines();
        assert_eq!(lines.len(), 1, "only refusals surface: {lines:?}");
        assert!(lines[0].contains("write src/main.rs"));
    }

    /// what this catches: an error type that collapses "cannot start" into
    /// "exchange failed". They point at different fixes — install the agent vs
    /// debug the protocol — and today's whole session was one long lesson in
    /// what a mislabelled cause costs.
    #[test]
    fn unavailable_and_exchange_failures_stay_distinguishable() {
        let unavailable = BridgeError::AgentUnavailable {
            command: "hermes-acp".into(),
            detail: "no such file".into(),
        };
        assert!(unavailable.to_string().contains("could not start"));
        assert!(unavailable.to_string().contains("uvx hermes-agent[acp]"));

        let exchange = BridgeError::Exchange {
            detail: "timeout".into(),
        };
        assert!(!exchange.to_string().contains("could not start"));
    }
}
