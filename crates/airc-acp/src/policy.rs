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
//!
//! ## Why the allow-list is keyed on KIND, not on tool name
//!
//! ACP's stable permission request identifies a tool by `ToolCallUpdateFields`,
//! whose `title` is a per-call human string ("Write file src/main.rs") and whose
//! `kind` is a fixed enum (`Read`/`Edit`/`Execute`/…). The *programmatic* name is
//! behind the `unstable_tool_call_name` feature.
//!
//! Allow-listing on `title` would therefore be matching free text that changes
//! per invocation — a rule that silently stops matching the moment an agent
//! rewords its own label. Allow-listing on `kind` is stable across agents and is
//! the decision an operator actually wants to express: *this agent may read, but
//! not execute*. So `kind` is the gate, and `title` is carried only for the human
//! reading the room line.
//!
//! See [`ToolIdentity::kind_key`] for the one place the enum becomes a config
//! string.

use std::collections::BTreeSet;

/// What the room knows about the tool an agent is asking to run.
///
/// Both fields are optional because *both are optional on the wire*. An agent
/// may request permission without declaring what for. That is not a reason to
/// assume it is harmless — see [`ToolPolicy::decide`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolIdentity {
    /// Normalized ACP `ToolKind` ("read", "execute", …), when the agent declared
    /// one. `None` means the agent did not say.
    pub kind: Option<String>,
    /// The agent's own human-readable label for this specific call. Display
    /// only — never matched against, because it varies per invocation.
    pub title: Option<String>,
}

impl ToolIdentity {
    /// The gate key: the declared kind, or `None` when the agent didn't declare.
    pub fn kind_key(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    /// How this tool call should read in a room line. Prefers the agent's own
    /// wording, falls back to the kind, and finally admits it doesn't know —
    /// rather than printing an empty string that reads like a bug.
    pub fn label(&self) -> String {
        match (self.title.as_deref(), self.kind.as_deref()) {
            (Some(t), Some(k)) => format!("{t} ({k})"),
            (Some(t), None) => t.to_string(),
            (None, Some(k)) => k.to_string(),
            (None, None) => "an undeclared tool".to_string(),
        }
    }
}

/// What the bridge decided about one tool-permission request, and why.
///
/// The reason is carried, not just the verdict, because it is published into the
/// room. "Denied" alone teaches the operator nothing; "denied: `execute` is not
/// in this account's toolsAllow" tells them exactly which knob to turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// The tool is explicitly allowed for this account.
    ///
    /// `caveat` is `Some` when the approval had to be granted on terms we would
    /// not have chosen — currently only when the agent offered no once-only
    /// option, so approving means it will remember. Carried rather than dropped
    /// because "the agent will stop asking" is exactly the kind of fact that
    /// must not be invisible.
    Allow {
        label: String,
        caveat: Option<String>,
    },
    /// Refused. `reason` is operator-facing and names the fix.
    Deny { label: String, reason: String },
}

impl PermissionDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PermissionDecision::Allow { .. })
    }

    /// One line, safe to publish into a room.
    pub fn to_room_line(&self) -> String {
        match self {
            PermissionDecision::Allow {
                label,
                caveat: None,
            } => {
                format!("permitted {label} (allow-listed)")
            }
            PermissionDecision::Allow {
                label,
                caveat: Some(c),
            } => {
                format!("permitted {label} (allow-listed) — {c}")
            }
            PermissionDecision::Deny { label, reason } => {
                format!("refused {label}: {reason}")
            }
        }
    }
}

/// Which classes of tool an ACP agent may run on behalf of a room.
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

    /// Permit exactly these tool kinds (`"read"`, `"search"`, `"execute"`, …).
    ///
    /// Names are normalized to lowercase so a config saying `"Read"` behaves as
    /// the operator plainly intended — a case mismatch silently denying a tool
    /// they explicitly allow-listed would be a wrong-shaped surprise.
    pub fn allow<I, S>(kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            allowed: kinds
                .into_iter()
                .map(|k| k.as_ref().trim().to_ascii_lowercase())
                .filter(|k| !k.is_empty())
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    /// Decide one request. Pure: no I/O, no logging, no side effects — so the
    /// rule is unit-testable without spawning an agent, and there is exactly one
    /// place the decision is made.
    pub fn decide(&self, tool: &ToolIdentity) -> PermissionDecision {
        let label = tool.label();

        // An agent that does not say what it wants to run does not thereby get to
        // run it. NOT-DECLARED and KNOWN-HARMLESS are different facts, and
        // collapsing them is how a gate ends up approving the one call it had the
        // least information about.
        let Some(kind) = tool.kind_key() else {
            return PermissionDecision::Deny {
                label,
                reason: "the agent did not declare a tool kind, so this cannot be matched against \
                         toolsAllow — an undeclared tool is refused, not assumed safe"
                    .to_string(),
            };
        };

        if self.allowed.contains(kind) {
            return PermissionDecision::Allow {
                label,
                caveat: None,
            };
        }

        let reason = if self.allowed.is_empty() {
            format!(
                "this account allow-lists no tools at all — set `toolsAllow` to permit kinds like \
                 `{kind}`"
            )
        } else {
            format!(
                "kind `{kind}` is not in this account's toolsAllow ({})",
                self.allowed.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        };
        PermissionDecision::Deny { label, reason }
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

    fn tool(kind: &str) -> ToolIdentity {
        ToolIdentity {
            kind: Some(kind.to_string()),
            title: None,
        }
    }

    /// what this catches: the SDK example's behaviour leaking into a room. An
    /// unconfigured lane must refuse tools, not inherit "allow everything" — in a
    /// room the prompter is an arbitrary peer.
    #[test]
    fn an_unconfigured_policy_permits_nothing() {
        let p = ToolPolicy::deny_all();
        let d = p.decide(&tool("execute"));
        assert!(!d.is_allowed());
        assert!(d.to_room_line().contains("refused execute"));
        // The refusal must name the fix, or the operator learns nothing from it.
        assert!(d.to_room_line().contains("toolsAllow"));
    }

    /// what this catches: treating "the agent didn't say" as "nothing to worry
    /// about". An undeclared tool carries the LEAST information, so it is the
    /// last one that should get the benefit of the doubt.
    #[test]
    fn an_undeclared_tool_kind_is_refused_even_when_tools_are_allowed() {
        let permissive = ToolPolicy::allow(["read", "execute"]);
        let undeclared = ToolIdentity {
            kind: None,
            title: Some("do the thing".into()),
        };
        let d = permissive.decide(&undeclared);
        assert!(!d.is_allowed(), "undeclared must not inherit an allow");
        assert!(d.to_room_line().contains("did not declare"), "{d:?}");
    }

    /// what this catches: a denial that says only "denied". The reason is
    /// published into the room; without the allow-list contents the operator
    /// cannot tell a typo from a policy choice.
    #[test]
    fn a_denial_names_what_was_allowed_instead() {
        let p = ToolPolicy::allow(["read", "search"]);
        let line = p.decide(&tool("execute")).to_room_line();
        assert!(line.contains("read"), "{line}");
        assert!(line.contains("search"), "{line}");
    }

    #[test]
    fn an_allow_listed_kind_is_permitted() {
        let p = ToolPolicy::allow(["read"]);
        assert!(p.decide(&tool("read")).is_allowed());
        assert!(
            !p.decide(&tool("read_all")).is_allowed(),
            "prefix must not match"
        );
    }

    /// what this catches: a case-sensitivity trap. An operator writing "Read" in
    /// their config means read; silently denying it would send them hunting
    /// through the bridge for a bug that is really a lowercase letter.
    #[test]
    fn config_casing_does_not_change_the_verdict() {
        let p = ToolPolicy::allow(["Read", "  EXECUTE  "]);
        assert!(p.decide(&tool("read")).is_allowed());
        assert!(p.decide(&tool("execute")).is_allowed());
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

    /// what this catches: an approval granted on terms we didn't choose going
    /// out silently. If the agent will REMEMBER an approval, the room has to be
    /// told, because from the next turn onward our policy stops being asked.
    #[test]
    fn an_approval_the_agent_will_remember_says_so_out_loud() {
        let d = PermissionDecision::Allow {
            label: "read (read)".into(),
            caveat: Some("the agent offered no once-only option, so it will remember".into()),
        };
        assert!(d.is_allowed());
        assert!(d.to_room_line().contains("will remember"), "{d:?}");
    }
}
