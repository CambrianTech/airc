# Hermes → AIRC adapter audit (first gap pass)

Closes work card **9ccb3146-af91-4f83-86ee-d9b956c9d6a4** (P1):
"Hermes adapter audit: map agent commands onto AIRC request/reply
contracts".

Phase-5 consumer proof. Companion to the OpenClaw audit
([`OPENCLAW-ADAPTER-AUDIT.md`](OPENCLAW-ADAPTER-AUDIT.md)), but
targeting Hermes' distinct shape: it's an agent runtime, not a
chat-platform aggregator. The audit focuses on **command model,
request/reply correlation, and capability advertisement** — what
the card explicitly asks for.

Repo audited: hermes-agent at
`/Users/joelteply/Development/opensource/hermes-agent` (commit at
audit time observable via `git -C <path> rev-parse HEAD`).

## What Hermes is

Python AI-agent runtime with three load-bearing surfaces:

- **Agent core** (`run_agent.py`, ~12k LOC) — `AIAgent` class
  drives the model conversation loop, tool orchestration, and
  streaming back to callers.
- **Tools** (`tools/registry.py`, `tools/*.py`,
  `model_tools.py`, `toolsets.py`) — each tool registers a JSON
  schema + handler at import time. Toolsets group tools for
  platform-specific scenarios.
- **Two outbound surfaces:**
  - **ACP adapter** (`acp_adapter/`) — exposes Hermes via the
    Agent Communication Protocol; clients connect, open a
    session, and receive typed session updates (events,
    plans, tool start/complete notifications).
  - **Messaging gateway** (`gateway/`) — `BasePlatformAdapter`
    abstract over 20+ chat platforms (telegram, discord,
    slack, signal, matrix, whatsapp, …). Outbound chat to a
    `chat_id` plus optional `reply_to`, capability flags per
    platform.

Internally Hermes' session state lives in SQLite
(`hermes_state.SessionDB`) with FTS5 search.

The distinction from OpenClaw matters: OpenClaw normalizes
*chat platforms* and hands a unified interface to plugins.
Hermes normalizes *agent capabilities* (tools) and hands them
to chat platforms. The AIRC adapter for Hermes therefore lives
on the **tool/capability** plane, not just the message plane.

## Mapping

| Hermes concept | Hermes type / path | AIRC concept | Notes |
|---|---|---|---|
| Tool registration | `tools/registry.py:register()` | AIRC structured capability event | Each tool's JSON schema + handler signature becomes a typed capability advertisement event; consumers subscribe by header |
| Toolset | `toolsets.py` (`get_toolset`, `resolve_toolset`) | Capability-event filter / tag | A toolset is a named bundle of tool ids; mirror as a `airc.hermes.toolset` header so subscribers filter without decoding the body |
| Tool call (request side) | ACP `tool_start` notification, see `acp_adapter/tools.py:build_tool_start` and `acp_adapter/events.py` | AIRC request frame: `FrameKind::Event` with structured body and correlation id | The `tool_call_id` (`make_tool_call_id` in `acp_adapter/tools.py`) is the correlation key; reuse it as the AIRC body's `request_id` field |
| Tool result (reply side) | ACP `tool_complete` notification, see `acp_adapter/tools.py:build_tool_complete` | AIRC reply frame with `reply_to: Some(request_event_id)` plus matching `request_id` | Two correlation layers — AIRC's envelope-level `reply_to` (fast filter) plus Hermes' explicit `request_id` (debuggable through replay) |
| ACP session | `acp_adapter/session.py` (`SessionDB`-backed) | AIRC `RoomId` | One ACP session = one AIRC room; the adapter derives a stable `RoomId` from the session UUID via `derive_room_id` |
| Platform adapter (telegram, discord, …) | `gateway/platforms/base.py:BasePlatformAdapter` | `ExternalIdentitySource` (#985) — bridge attribution surface | Each Hermes platform plugin already maps onto exactly the bridge-source enum AIRC ships |
| Inbound chat → agent | `BasePlatformAdapter.connect()` event loop, message processing in `gateway/platforms/base.py:_process_message` | AIRC `Message`/`Event` frame consumed by the adapter, decoded into Hermes' internal message shape | Adapter is reverse direction of #985's bridge contract |
| Outbound agent → chat | `BasePlatformAdapter.send(chat_id, content, reply_to, metadata)` | AIRC `Airc::publish` (#990) routing back to the room the inbound originated from | Use `PublishTarget::RoomByName` to route; the per-room mapping is already in the session DB |
| Streaming response chunks | AIAgent callbacks → ACP `session_update()` (`acp_adapter/events.py`) | AIRC ephemeral event stream (subscribe path) | Streaming chunks are high-rate; they should NOT cause an `airc.diag` poison if the consumer is slow — use header `airc.hermes.stream=chunk` and let subscribers drop on back-pressure |

## Command model (request side)

Hermes' tool-call model is naturally request/reply:

1. Model emits a function/tool call.
2. `model_tools.handle_function_call` dispatches to the
   registered handler in `tools/registry.py`.
3. Handler returns a result (or yields chunks for streaming
   tools).
4. ACP emits `tool_start` → handler runs → `tool_complete`.

For AIRC, this maps onto a request envelope:

```json
{
  "kind": "hermes.tool.request",
  "tool": "search_files",
  "request_id": "<tool_call_id>",
  "args": { "query": "foo" }
}
```

Reply envelope (AIRC frame with `reply_to: request_event_id`):

```json
{
  "kind": "hermes.tool.reply",
  "request_id": "<tool_call_id>",
  "ok": true,
  "result": { "matches": [...] }
}
```

Both envelopes go as `Body::Json` on `FrameKind::Event`.
Filterable headers: `airc.hermes.tool` (tool name),
`airc.hermes.request_id` (correlation), `airc.hermes.kind`
(`request|reply|chunk`).

## Capability advertisement

Hermes already has a structured tool registry — the work is
*serializing it onto AIRC*, not redesigning it. Two emit
points:

1. **On adapter startup** — bridge process publishes one
   `capability` event per tool the local Hermes installation
   exposes:

   ```json
   {
     "kind": "hermes.capability.tool",
     "tool": "search_files",
     "schema": { ...JSON schema from registry... },
     "toolsets": ["research", "full_stack"],
     "available": true
   }
   ```

2. **On dynamic toolset change** — when an ACP session opens
   with a specific toolset, emit an `airc.hermes.toolset`
   header on subsequent requests so consumers see which subset
   is in scope.

Subscribers can `subscribe_filtered(EventFilter::with_header(
"airc.hermes.kind", "capability.tool"))` to build a live
capability board across the mesh.

## Adapter boundary

Same two shapes as OpenClaw, with the same conclusion: **Shape
B (external Rust bridge process)** for the first slice.

Hermes-specific reasons:

- Hermes is Python; in-process linking to `airc-lib` requires
  PyO3 or a child process. Both are doable but neither is the
  *first* slice.
- Hermes' plugin system already supports `register_platform()`
  for messaging adapters. An AIRC platform plugin
  (`gateway/platforms/airc.py`) IS a defensible Shape A —
  AIRC becomes "just another chat platform" from Hermes'
  perspective, with `chat_id` mapping to AIRC `RoomId` and
  `send()` calling `airc publish` via subprocess. Once Shape B
  proves out, this is the natural next step.
- Tool-call request/reply is **not** the chat platform's job —
  it's the agent core's. So the bridge process can't just be a
  chat-platform plugin; it needs its own seam directly into
  the agent loop. The simplest such seam: subscribe to ACP
  session updates over Hermes' own ACP server (already typed,
  already a stable interface) and republish onto AIRC.

**Recommendation:** Shape B bridge subscribes to ACP, not to
the gateway. ACP is already the structured surface; bridging
ACP → AIRC reuses Hermes' own contract instead of reinventing
inside the chat-platform plugin model.

## Smallest fixture proof

End-to-end, no live remote services:

1. **Start Hermes' ACP server locally** in test mode (Hermes
   already has live test harnesses for ACP under
   `acp_adapter/`).
2. **Bridge process** connects to ACP as a client. Subscribes
   to all session updates.
3. **Open an ACP session** with a tiny toolset (e.g. just
   `echo_tool` or `get_time`).
4. **Send a synthetic prompt** that causes a single tool call.
   ACP emits:
   - `tool_start` for that tool call.
   - `tool_complete` after the handler returns.
5. **Bridge republishes** both to AIRC:
   - `tool_start` → AIRC request frame with `request_id =
     tool_call_id`.
   - `tool_complete` → AIRC reply frame with `reply_to =
     <request_event_id>` AND matching `request_id`.
6. **Second AIRC peer** subscribes via
   `subscribe_filtered(EventFilter::with_header(
   "airc.hermes.kind", "reply"))` and observes the reply,
   correlates back to the request via `request_id`.

Pass criterion:

- Reply's AIRC `reply_to` field cryptographically links to the
  request's `event_id`.
- Bodies' `request_id` matches the original tool_call_id.
- A subscriber filtering only by `tool=<name>` sees both
  request and reply without decoding bodies.

## Gaps that block the next slice

1. **No structured request/reply primitive in AIRC.** Today
   AIRC has `Airc::publish` (#990) for one-way and the
   envelope's `reply_to` for chain-of-replies, but no typed
   `Airc::request(target, body) -> Future<Reply>` helper.
   Consumers can compose it from publish + subscribe with a
   correlation header, and that's fine for the first slice,
   but a typed helper would close the loop and fits the
   "verbs = interfaces" rule from the user's CLAUDE.md.
   **Carry as a follow-up card.**
2. **Tool schemas can be large.** Capability events with full
   JSON schemas may exceed the typical frame size for
   high-cardinality tool registries. AIRC already has the
   blobs-on-disk discipline (see project memory:
   `feedback_blobs_never_in_db`); capability schemas should
   live on disk with a content-hash pointer in the AIRC
   event. **Carry as follow-up.**
3. **Streaming chunks need back-pressure semantics.** Hermes'
   ACP streams partial completions at high rate. AIRC's
   publish API doesn't currently expose a `drop_if_slow`
   knob like OpenClaw's `broadcastPresenceSnapshot`. Without
   it, a slow subscriber wedges the bridge. The right shape
   may be the `Ephemeral` frame kind from card `ac3f1b36`
   ("Ephemeral event kind for diagnostics and high-rate
   presence"). **Wait for ac3f1b36; treat as dependency.**
4. **Hermes session id ↔ AIRC RoomId stability across
   restarts.** Hermes persists sessions to SQLite and resumes
   them by id. The bridge must keep the same `RoomId` across
   bridge process restarts. Document the derivation:
   `RoomId = derive_room_id(MeshIdentity::unset(),
   ChannelName::new(format!("hermes-session-{session_id}")))`.
   Belongs in this PR's mapping table; no follow-up.

## Non-scope (explicitly deferred)

- **Inbound chat → agent via AIRC.** This audit covers the
  AIRC-as-observer direction (subscribers see what Hermes is
  doing). The reverse direction — sending a prompt to Hermes
  *from* AIRC — needs the `airc.hermes.kind=request` path
  AND a Hermes-side AIRC platform plugin (Shape A) to deliver
  it into the agent core. **Carry as follow-up.**
- **Multiple concurrent ACP sessions per bridge.** First
  slice handles one session at a time. Multi-session is
  bookkeeping inside the bridge, not an AIRC primitive
  concern.
- **Authentication / authorization.** AIRC has cryptographic
  per-peer identity; Hermes' ACP has its own auth
  (`acp_adapter/auth.py`). Reconciling them — e.g., when a
  Hermes ACP client should be allowed to subscribe to a given
  AIRC room — is policy, not adapter. **Carry as follow-up.**
- **Plugin platforms** (telegram, discord, …) **routed
  through AIRC.** Each Hermes platform plugin could be
  bridged to AIRC the same way OpenClaw's `extensions/*`
  are. That's an aggregation problem outside this card;
  Hermes-via-AIRC is the goal here, not platform-via-AIRC.

## Cross-references

- AIRC bridge contract:
  [PR #985](https://github.com/CambrianTech/airc/pull/985),
  module `airc_lib::external_identity`.
- AIRC structured publish:
  [PR #990](https://github.com/CambrianTech/airc/pull/990),
  module `airc_lib::publish`.
- OpenClaw audit (sibling Phase-5 doc):
  [`OPENCLAW-ADAPTER-AUDIT.md`](OPENCLAW-ADAPTER-AUDIT.md).
- Consumer matrix anchor:
  [`CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md`](CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md).
- Hermes architecture: hermes-agent repo root `AGENTS.md` +
  `acp_adapter/` and `gateway/platforms/base.py`.
