//! Runtime context classification for public command behavior.
//!
//! `airc join` has one public UX, but the process it runs inside
//! matters: an agent/interactive process should stream, while tests and
//! automation must complete. Keep that classification out of command
//! handlers so consumers do not grow ad hoc Codex/Claude branches.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRuntimeKind {
    Claude,
    Codex,
    Generic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeContext {
    InteractiveTerminal,
    Agent {
        kind: AgentRuntimeKind,
        client_id: Option<String>,
    },
    Automation,
    TestHarness,
}

impl RuntimeContext {
    pub fn current() -> Self {
        use std::io::IsTerminal;

        let runtime_client = crate::client_id::current_client_id().ok().flatten();
        classify(
            std::env::vars_os().map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            }),
            std::io::stdout().is_terminal(),
            runtime_client,
        )
    }

    pub fn should_stream_join(&self) -> bool {
        matches!(self, Self::InteractiveTerminal | Self::Agent { .. })
    }
}

fn classify<I, K, V>(env: I, stdout_is_tty: bool, runtime_client: Option<String>) -> RuntimeContext
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut saw_opt_out = false;
    let mut saw_cargo_context = false;
    let mut agent_marker = None;

    for (key, _value) in env {
        match key.as_ref() {
            "AIRC_NO_ATTACH" => saw_opt_out = true,
            "CARGO_PKG_NAME" => saw_cargo_context = true,
            "CLAUDECODE" | "CLAUDE_CODE_SESSION_ID" => {
                agent_marker.get_or_insert(AgentRuntimeKind::Claude);
            }
            "CODEX_AGENT_ID" | "CODEX_SESSION_ID" | "AIRC_CODEX_START_CHILD" => {
                agent_marker.get_or_insert(AgentRuntimeKind::Codex);
            }
            "AI_AGENT" => {
                agent_marker.get_or_insert(AgentRuntimeKind::Generic);
            }
            key if key.starts_with("CARGO_BIN_EXE_") => saw_cargo_context = true,
            _ => {}
        }
    }

    // Priority order is intentional:
    // - explicit opt-out and cargo harnesses must not hang;
    // - agent runtimes can have piped stdout and still need the stream;
    // - direct TTY users should stream;
    // - plain scripts return after setup.
    if saw_opt_out {
        return RuntimeContext::Automation;
    }
    if saw_cargo_context {
        return RuntimeContext::TestHarness;
    }
    if let Some(kind) = agent_marker {
        return RuntimeContext::Agent {
            kind,
            client_id: runtime_client,
        };
    }
    if let Some(client_id) = runtime_client {
        return RuntimeContext::Agent {
            kind: kind_from_client_id(&client_id),
            client_id: Some(client_id),
        };
    }
    if stdout_is_tty {
        RuntimeContext::InteractiveTerminal
    } else {
        RuntimeContext::Automation
    }
}

fn kind_from_client_id(client_id: &str) -> AgentRuntimeKind {
    if client_id.starts_with("claude:") {
        AgentRuntimeKind::Claude
    } else if client_id.starts_with("codex:") {
        AgentRuntimeKind::Codex
    } else {
        AgentRuntimeKind::Generic
    }
}

#[cfg(test)]
pub(crate) fn classify_for_test<I, K, V>(
    env: I,
    stdout_is_tty: bool,
    runtime_client: Option<&str>,
) -> RuntimeContext
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    classify(env, stdout_is_tty, runtime_client.map(ToString::to_string))
}

#[cfg(test)]
mod tests {
    use super::{classify_for_test, AgentRuntimeKind, RuntimeContext};

    #[test]
    fn join_streams_for_codex_agent() {
        let context = classify_for_test([("CODEX_SESSION_ID", "thread-1")], false, None);
        assert_eq!(
            context,
            RuntimeContext::Agent {
                kind: AgentRuntimeKind::Codex,
                client_id: None
            }
        );
        assert!(context.should_stream_join());
    }

    #[test]
    fn join_streams_for_claude_agent() {
        let context = classify_for_test([("CLAUDE_CODE_SESSION_ID", "session-1")], false, None);
        assert_eq!(
            context,
            RuntimeContext::Agent {
                kind: AgentRuntimeKind::Claude,
                client_id: None
            }
        );
        assert!(context.should_stream_join());
    }

    #[test]
    fn join_streams_for_interactive_tty() {
        let context = classify_for_test(std::iter::empty::<(&str, &str)>(), true, None);
        assert_eq!(context, RuntimeContext::InteractiveTerminal);
        assert!(context.should_stream_join());
    }

    #[test]
    fn join_streams_for_detected_runtime_client() {
        let context = classify_for_test(
            std::iter::empty::<(&str, &str)>(),
            false,
            Some("codex:thread-1"),
        );
        assert_eq!(
            context,
            RuntimeContext::Agent {
                kind: AgentRuntimeKind::Codex,
                client_id: Some("codex:thread-1".to_string())
            }
        );
        assert!(context.should_stream_join());
    }

    #[test]
    fn join_streams_for_monitor_piped_stdout() {
        let codex = classify_for_test([("CODEX_SESSION_ID", "thread-1")], false, None);
        assert!(codex.should_stream_join());

        let claude = classify_for_test([("CLAUDE_CODE_SESSION_ID", "session-1")], false, None);
        assert!(claude.should_stream_join());
    }

    #[test]
    fn join_exits_for_plain_pipe_with_no_agent_context() {
        let context = classify_for_test(std::iter::empty::<(&str, &str)>(), false, None);
        assert_eq!(context, RuntimeContext::Automation);
        assert!(!context.should_stream_join());
    }

    #[test]
    fn join_exits_for_cargo_context() {
        let context = classify_for_test(
            [
                ("CLAUDE_CODE_SESSION_ID", "session-1"),
                ("CARGO_BIN_EXE_airc", "/tmp/airc"),
            ],
            false,
            None,
        );
        assert_eq!(context, RuntimeContext::TestHarness);
        assert!(!context.should_stream_join());
    }

    #[test]
    fn join_internal_opt_out_wins() {
        let context = classify_for_test(
            [("CODEX_SESSION_ID", "thread-1"), ("AIRC_NO_ATTACH", "1")],
            true,
            Some("codex:thread-1"),
        );
        assert_eq!(context, RuntimeContext::Automation);
        assert!(!context.should_stream_join());
    }

    #[test]
    fn detected_unknown_runtime_client_is_generic_agent() {
        let context =
            classify_for_test(std::iter::empty::<(&str, &str)>(), false, Some("tool:one"));
        assert_eq!(
            context,
            RuntimeContext::Agent {
                kind: AgentRuntimeKind::Generic,
                client_id: Some("tool:one".to_string())
            }
        );
        assert!(context.should_stream_join());
    }
}
