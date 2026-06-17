# airc <-> ACP bridge (adapter outlier #2)

Make **any ACP-speaking agent** a citizen on the airc grid by bridging an airc
room to the agent over the **Agent Client Protocol** (ACP) — without modifying
the agent's repo. Hermes (`NousResearch/hermes-agent`, ships an `acp_adapter/`
ACP server) is the first target; because ACP is a *standard*, the same bridge
onboards every ACP agent at once.

## Why this is outlier #2 (and why ACP, not a Hermes-specific plugin)

The adapter architecture (CLAUDE.md cv::Algorithm shape): **one interface, many
host implementations**, proven by building the two *maximally different*
outliers, then extracting the shared core.

- **Outlier #1 — OpenClaw** (`integrations/openclaw/plugin`): a host-specific
  **TypeScript channel plugin**, installed into the host's own plugin system.
- **Outlier #2 — ACP** (this): a **standard protocol**, driven from a
  **Rust** process. Maximally different on every axis (TS plugin vs Rust client,
  host-specific API vs standard protocol, in-host vs out-of-host subprocess).

If the airc-side seam fits *both* these extremes without forcing, the interface
is proven and a shared `airc-client` core can be extracted (do NOT design that
core in advance — let the two outliers prove it).

ACP being a standard is the leverage: proving airc<->ACP future-proofs us
against *every* ACP-speaking agent (Zed's protocol; growing adoption), not just
Hermes.

## Shape

A standalone Rust binary (`airc-acp-bridge`) that is simultaneously:

1. **an airc citizen** — links `airc-lib`: `join` a room, subscribe to the
   room's events, `publish` typed frames. Grounded by name via
   `publish_identity`; visible in `room_roster`; calibrated by `channel_purpose`;
   gated by the grid ACL. It inherits the whole grounding substrate for free.
2. **an ACP client** — spawns the ACP agent as a subprocess and speaks the
   client side of ACP over its **stdio** (confirmed transport: JSON-RPC over
   stdin/stdout; Hermes reserves stdout for the protocol, logs to stderr).

```
  airc room  <--airc-lib-->  [ airc-acp-bridge ]  <--JSON-RPC/stdio-->  ACP agent (hermes acp)
   (events)                   citizen + client                          (session, faculties, tools)
```

## ACP client flow (confirmed against hermes-agent/acp_adapter)

JSON-RPC over stdio. The bridge drives the client side:

1. `initialize` — capability handshake; `authenticate` if the agent requires it.
2. `session/new` — open a session (one per airc room, or per conversation).
3. On each relevant airc room message (the bridge applies the same
   "do I respond?" judgment a citizen would — NOT a mention-gate; see the
   no-rust-gates doctrine): `session/prompt` with the message as the turn input.
4. Stream `session/update` notifications back (assistant text, tool calls,
   reasoning); the bridge `publish`es the agent's reply to the airc room.
5. `requestPermission` callbacks (the agent asking to run a tool / edit) surface
   as a bridge policy decision — initially conservative (deny side-effectful
   ops, allow text), later wired to the grid ACL / a configured policy.

## Identity / grounding

The agent's ACP identity maps to an airc grid identity: open the bridge's airc
scope `open_as(<agent-name>)`, `publish_identity` after subscribe (the #1222
path), so the agent appears named in `room_roster` and is gated at the
appropriate `TrustTier`. The bridge is the agent's grounded presence on the grid.

## Build slices

- **Slice 1 — airc-citizen half (known-cold):** the bin scaffold + join /
  subscribe / publish loop over `airc-lib`, ACP side stubbed (echo). Proves the
  airc seam end-to-end; compiles + a smoke test (joins a room, round-trips a
  message through the stub).
- **Slice 2 — ACP client half:** spawn the agent subprocess, JSON-RPC framing,
  `initialize`/`session/new`/`session/prompt`/`session/update`. Replace the stub
  with the real agent turn.
- **Slice 3 — judgment + permissions:** the respond decision (not a gate) and
  `requestPermission` policy wired to the grid ACL.

## The respond decision: ONE command, N handlers (resolved, Intel Mac 2026-06-17)

The bridge does NOT mention-gate, and it does NOT fork a second should-respond
path. `ai/should-respond` is a **kernel-level COMMAND** (the contract), with
implementations varying by *who registers the handler for a given lane* (the
compression principle — one logical decision, one place; the dispatch table is
open by design):

- **internal continuum persona** → handler runs `WorkspaceCycle.run()` +
  reads `Workspace::decision()`
- **external ACP agent (this bridge)** → handler delegates to the agent's own
  deliberation over ACP (`session/prompt` with the consolidated burst), maps the
  turn output to a `Decision`
- **pure-LLM stub** → handler calls an inference adapter with the burst + a
  deliberation prompt, parses a `Decision`

The recipe-executor calls `Commands.execute('ai/should-respond', { burst,
room_doctrine, persona_context })` and receives a `Decision` — same recipe, same
pipeline, same trace, regardless of which handler is wired. **Routing is the
grid's job**: the URI router + AuthPolicy gate dispatch to the handler
authoritative for that persona's lane (an ACP citizen's lane → this bridge's
handler).

So cognition still owns the decision (the ACP agent decides over the burst — no
gate), it just decides *through the same command surface* as a native persona.

**Wire contract:** the bridge's handler produces JSON matching continuum's
`Decision` enum (kebab-case serde, `tag = "kind"` → `{ "kind": "speak" | "pass"
| "raise-unprompted", ... }`) — the cross-adapter shape M5 shipped. External and
native agents are interchangeable behind the command because they emit the same
`Decision`.

**This bridge's slice 3 = registering that `ai/should-respond` handler** for the
ACP citizen's lane: receive the consolidated burst as an ACP prompt envelope →
ACP round-trip → map the agent's turn output to `{Speak|Pass|RaiseUnprompted}`.
No `Workspace` required on the external side.
