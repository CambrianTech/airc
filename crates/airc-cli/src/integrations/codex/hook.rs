//! `airc codex-hook ...`: Codex's lines over the shared hook core
//! (`integrations::agent_hook`).

use crate::integrations::agent_hook::HookRuntime;

pub const CODEX: HookRuntime = HookRuntime {
    verb: "codex-hook",
    name: "Codex",
    session_prefix: "codex:",
    presence: "codex",
    wakes: false,
};
