//! Tool-permission policy for an ACP agent answering in a ROOM.
//!
//! ## Why this is not the SDK's example
//!
//! The `agent-client-protocol` SDK ships a client example that auto-approves
//! every permission request. It is honest about that — it is named
//! `yolo_one_shot_client`, and for its use case it is right: a human typed the
//! prompt into their own terminal and is watching the tool run.
//!
//! A room inverts the trust model. The prompt arrives from a PEER, and airc
//! broadcasts carry no structural addressing (every send is `MentionTarget::All`,
//! see the "What Doesn't Work Yet" section of the README), so *anyone* who can
//! reach the room is a potential prompter. Auto-approving there would mean any
//! peer can induce arbitrary tool execution — file writes, shell, network — on
//! the machine hosting the agent, with no human in the loop.
//!
//! So the default here is DENY, and the denial is meant to be spoken aloud into
//! the room rather than swallowed. A silent denial is its own bug: the agent
//! stalls, the room sees nothing, and whoever debugs it spends an hour before
//! discovering a permission gate declined without saying so.

use std::collections::BTreeSet;

/// What the bridge decided about one tool-permission request, and why.
///
/// The reason is carried, not just the verdict, because it is published into the
/// room. "Denied" alone teaches the operator nothing; "denied: `write_file` is not
/// in this account's toolsAllow" tells them exactly which knob to turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// The tool is explicitly allowed for this account.
    Allow { tool: String },
    /// Refused. `reason` is operator-facing and names the fix.
    Deny { tool: String, reason: String },
}

impl PermissionDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PermissionDecision::Allow { .. })
    }

    /// One line, safe to publish into a room.
    pub fn to_room_line(&self) -> String {
        match self {
            PermissionDecision::Allow { tool } => {
                format!("permitted tool `{tool}` (allow-listed)")
            }
            PermissionDecision::Deny { tool, reason } => {
                format!("refused tool `{tool}`: {reason}")
            }
        }
    }
}

/// Which tools an ACP agent may run on behalf of a room.
///
/// Empty means empty: an account that lists no tools gets no tools. This is
/// deliberately NOT "unset means everything" — the permissive reading of an
/// absent config is how a security default becomes an accident.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    allowed: BTreeSet<String>,
}

impl ToolPolicy {
    /// A policy that permits nothing. The correct default for a lane that has
    /// not been configured: it fails closed and says so, rather than deciding on
    /// the operator's behalf that their unconfigured agent should have a shell.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Permit exactly these tool names.
    pub fn allow<I, S>(tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed: tools.into_iter().map(Into::into).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    /// Decide one request. Pure: no I/O, no logging, no side effects — so the
    /// rule is unit-testable without spawning an agent, and there is exactly one
    /// place the decision is made.
    pub fn decide(&self, tool: &str) -> PermissionDecision {
        if self.allowed.contains(tool) {
            return PermissionDecision::Allow {
                tool: tool.to_string(),
            };
        }
        let reason = if self.allowed.is_empty() {
            "this account allow-lists no tools at all — set `toolsAllow` to permit specific ones"
                .to_string()
        } else {
            format!(
                "not in this account's toolsAllow ({})",
                self.allowed.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        };
        PermissionDecision::Deny {
            tool: tool.to_string(),
            reason,
        }
    }
}

/// Who may prompt this agent at all.
///
/// Separate from [`ToolPolicy`] on purpose: "may this peer talk to the agent"
/// and "may the agent run this tool" are different questions, and collapsing
/// them produces a config where tightening one silently loosens the other.
#[derive(Debug, Clone, Default)]
pub struct PeerPolicy {
    allowed: BTreeSet<String>,
}

impl PeerPolicy {
    /// Accept any peer in the room. This IS the sane default for prompting —
    /// unlike tools, a prompt runs no code; it only asks the agent to think. The
    /// tool gate is where the teeth belong.
    pub fn any() -> Self {
        Self::default()
    }

    pub fn only<I, S>(peers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed: peers.into_iter().map(Into::into).collect(),
        }
    }

    pub fn may_prompt(&self, peer_id: &str) -> bool {
        self.allowed.is_empty() || self.allowed.contains(peer_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches: the SDK example's behaviour leaking into a room. An
    /// unconfigured lane must refuse tools, not inherit "allow everything" — in a
    /// room the prompter is an arbitrary peer.
    #[test]
    fn an_unconfigured_policy_permits_nothing() {
        let p = ToolPolicy::deny_all();
        let d = p.decide("shell");
        assert!(!d.is_allowed());
        assert!(d.to_room_line().contains("refused tool `shell`"));
        // The refusal must name the fix, or the operator learns nothing from it.
        assert!(d.to_room_line().contains("toolsAllow"));
    }

    /// what this catches: a denial that says only "denied". The reason is
    /// published into the room; without the allow-list contents the operator
    /// cannot tell a typo from a policy choice.
    #[test]
    fn a_denial_names_what_was_allowed_instead() {
        let p = ToolPolicy::allow(["read_file", "search"]);
        let line = p.decide("write_file").to_room_line();
        assert!(line.contains("read_file"), "{line}");
        assert!(line.contains("search"), "{line}");
    }

    #[test]
    fn an_allow_listed_tool_is_permitted() {
        let p = ToolPolicy::allow(["read_file"]);
        assert!(p.decide("read_file").is_allowed());
        assert!(
            !p.decide("read_file2").is_allowed(),
            "prefix must not match"
        );
    }

    /// what this catches: collapsing the two gates. Prompting is open by default
    /// (a prompt runs no code); tools are closed by default. If a refactor ever
    /// makes these share one set, this fails.
    #[test]
    fn prompting_is_open_by_default_while_tools_are_closed() {
        assert!(PeerPolicy::any().may_prompt("peer-anyone"));
        assert!(ToolPolicy::deny_all().is_empty());
    }

    #[test]
    fn an_explicit_peer_list_excludes_everyone_else() {
        let p = PeerPolicy::only(["peer-a"]);
        assert!(p.may_prompt("peer-a"));
        assert!(!p.may_prompt("peer-b"));
    }
}
