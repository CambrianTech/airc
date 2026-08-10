# airc as an ACP client — one bridge, N agents

**Status:** transport implemented and covered by in-process round-trip tests
(2026-08-09). Live subprocess round-trip still outstanding — see
[Verification bar](#verification-bar).

## Where the code lives (read this before adding a second ACP path)

There are two pieces and they compose. An earlier version of this document
described only the library and did not mention the binary, which is how you end
up with two ACP efforts instead of one:

| Piece | What it is | State |
|---|---|---|
| `crates/airc-acp` | The **library**: policy + transport. Spawns an agent, drives one turn, returns an `AgentTurn`. Knows nothing about rooms. | policy + transport landed |
| `integrations/acp` (`airc-acp-bridge`) | The **binary**: an airc citizen (join / subscribe / publish via `airc-lib`) that calls the library for each inbound message. | slice 1 landed (citizen loop with a stub `TurnHandler`); slice 2 = swap the stub for `AcpBridge` |

`integrations/acp/README.md` planned slice 2 as hand-written JSON-RPC framing over
stdio. That is superseded: we use the published `agent-client-protocol` SDK
instead. Framing, batching, cancellation, and protocol-version negotiation are
things the SDK already gets right and that a hand-rolled framer would get subtly
wrong — and "subtly wrong protocol framing" is a bug that presents as an agent
that mysteriously goes quiet.

## Why this shape, and not a Hermes plugin

The obvious move was "write an airc plugin for Hermes", mirroring what we did for
OpenClaw. Reading Hermes first killed that idea, in the good way:
`hermes-agent` already ships `acp_adapter/` with an `agent-client-protocol`
dependency, JSON-RPC over stdio, and a standard `acp_registry/agent.json`
declaring `uvx hermes-agent[acp]` distribution. It is **already an ACP agent**.

So it needs nothing from us. And a Hermes-specific plugin would buy one agent
while coupling us to their internals forever.

Making **airc an ACP client** instead inverts it: every agent that ships an ACP
adapter becomes a room citizen, with zero upstream patches and nothing for us to
maintain inside anyone else's codebase. Hermes today; the rest of the registry
for free.

That gives the on-ramp two classes, which is worth stating plainly because they
are not interchangeable:

| Class | Direction | Use when | Example |
|---|---|---|---|
| **airc as ACP client** | airc drives the agent | the agent speaks a standard | Hermes, any `acp_registry` entry |
| **channel plugin** | the host drives airc | the *host* owns the UI/UX surface | OpenClaw (`integrations/openclaw/plugin`) |

OpenClaw is not an ACP agent — it is a chat *host*, so it stays a channel plugin.
Hermes is an agent, so it needs no plugin at all. Picking the wrong class means
either patching upstream forever or reimplementing someone's UI.

## The contract (read off the SDK, not guessed)

`agent-client-protocol = "2.0.0"` (Apache-2.0, `agentclientprotocol/rust-sdk`,
rust-version 1.88). Companion: `agent-client-protocol-tokio`.

The client flow, from the SDK's own `examples/yolo_one_shot_client.rs`:

```rust
let agent = AcpAgent::from_str("hermes-acp")?;   // spawns it; implements ConnectTo

Client.builder()
    .on_receive_notification(|n: SessionNotification, _cx| async { /* stream */ },
                             on_receive_notification!())
    .on_receive_request(|r: RequestPermissionRequest, responder, _conn| async { /* policy */ },
                        on_receive_request!())
    .connect_with(agent, |conn: ConnectionTo<Agent>| async move {
        conn.send_request(InitializeRequest::new(ProtocolVersion::V1)).block_task().await?;
        let s = conn.send_request(NewSessionRequest::new(cwd)).block_task().await?;
        conn.send_request(PromptRequest::new(s.session_id,
                vec![ContentBlock::Text(TextContent::new(text))])).block_task().await?;
        Ok(())
    }).await?;
```

Mapping onto a room:

| ACP | airc |
|---|---|
| `PromptRequest` | an inbound room message routed to this agent |
| `SessionNotification` updates | the agent's reply, published back to the room |
| `RequestPermissionRequest` | a **policy decision** — see below |
| `session_id` | one per (room, agent) pair, so context is per-room |

## Permission policy: NOT the example's

The SDK example auto-approves every permission request and is honest about it —
it is called `yolo_one_shot_client`. That is correct for a one-shot CLI where the
human typed the prompt themselves.

It is **wrong for a room**, and would be our bug, not the SDK's. In a room the
prompt comes from a *peer*, and airc broadcasts carry no structural addressing
(every send is `MentionTarget::All`). Auto-approving would mean any peer who can
reach the room can induce arbitrary tool execution on this machine — file writes,
shell, network — with no human in the loop.

So the bridge's default is **deny, visibly**:

- default: deny the permission request AND publish the denial into the room, so
  the refusal is legible rather than a silent stall the agent's author will spend
  an hour debugging;
- an explicit per-account `toolsAllow` list may auto-approve *named* tools;
- an explicit `allowFrom` peer list gates who can prompt at all;
- nothing is approved because it "seemed fine" — an unlisted tool is denied and
  said out loud.

This mirrors the OpenClaw plugin's config surface (`toolsAllow`, `allowFrom`),
which is deliberate: one mental model across both on-ramp classes.

### `toolsAllow` lists KINDS, not tool names (found while building)

The stable ACP permission request identifies a tool by `ToolCallUpdateFields`:
`title` is a per-call human string ("Write file src/main.rs"), `kind` is a fixed
enum. The *programmatic* name is behind the `unstable_tool_call_name` feature.

So allow-listing by name would mean matching free text that changes per
invocation — a rule that silently stops matching when an agent rewords its own
label. `kind` is stable across agents and is the decision an operator actually
wants to make. The vocabulary is exactly ACP's `ToolKind`:

```
read  edit  delete  move  search  execute  think  fetch  switch_mode  other
```

Two consequences worth stating because both are load-bearing:

- **`other` is ACP's default** for an unclassified tool, and it is also where any
  *future* `ToolKind` variant lands (the enum is `#[non_exhaustive]`). So a new
  protocol capability arrives **closed**, not open.
- **Both `kind` and `title` are `Option`.** An agent may request permission
  without declaring what for. That is refused, explicitly: "did not declare" and
  "declared something harmless" are different facts, and the request carrying the
  *least* information is the last one that should get the benefit of the doubt.

### Answer `AllowOnce`, never `AllowAlways`

Permission options come in once/always pairs. Replying `AllowAlways` tells the
agent to remember the grant and **stop sending permission requests** — which would
leave `ToolPolicy` wired up, passing all its tests, and never consulted again.
That is the silently-unwired-capability failure in miniature, arrived at by
choosing the obvious-looking option.

The bridge therefore selects by *kind*, preferring the once-scoped option in both
directions. (The SDK example picks `options.first()`; option order is the agent's
choice, so "first" can mean "always".) If an agent offers only an always-scoped
option we still answer — the policy did say yes — but the room is told the terms
changed. If it offers no option of the needed polarity, the request is
`Cancelled`, which is the protocol's own word for "no", rather than guessing at an
option of the wrong polarity.

## Open questions to settle while building

- **Session lifetime.** One session per room forever, or per conversation burst?
  Forever grows context unboundedly; per-burst loses continuity. Probably
  per-room with an idle reset, but that is a measurement, not a guess.
- **Streaming granularity.** `SessionNotification` arrives token-ish; airc
  messages are discrete. Publishing every update would flood the room (and hit
  the body-clip gap, #378). Likely: buffer to completion, publish once, with a
  `typing`-style presence signal meanwhile.
- **Agent liveness.** A spawned agent that dies must be reported to the room, not
  silently stop answering — the same "up is not the same as talking" rule the
  README now leads with.
- **Windows.** `hermes-acp` reserves stdout for JSON-RPC and mentions UTF-8 stdio
  bootstrapping on Windows specifically. Test there first; it is the platform
  that has broken every cross-platform assumption this week.

## Verification bar

Not "it compiles". A real receipt:

| # | Claim | State |
|---|---|---|
| 1 | An agent is spawned as a subprocess and speaks ACP over stdio | ✅ `tests/subprocess_round_trip.rs`, on Windows |
| 2 | A prompt reaches the agent and its reply comes back assembled | ✅ in-process + subprocess |
| 3 | A tool request with no `toolsAllow` entry is denied, the denial crosses the process boundary, and it is room-visible | ✅ subprocess test asserts both halves |
| 4 | An agent that dies mid-turn produces an error, not an empty success | ✅ `an_agent_that_dies_mid_turn_is_an_error_not_silence` |
| 5 | A missing agent binary is reported as *unavailable*, distinctly from a protocol failure | ✅ |
| 6 | The reply is published into a real room and visible to a **second peer** | ❌ **not yet** — needs two live airc peers |
| 7 | A real third-party agent (`uvx hermes-agent[acp]`) works, not just our fixture | ❌ **not yet** |

Rows 1–5 are covered by tests that run on every platform in CI, using an in-repo
fixture agent (`acp-echo-agent`, gated behind the `test-fixtures` feature) rather
than a network download — so the subprocess path is *regression-protected*, not
merely demonstrated once.

Rows 6 and 7 are the honest gaps, and they are different in kind:

- **Row 6** is the airc half — the library returns the right thing, and
  `integrations/acp` publishes it, but nobody has watched a second peer receive
  it. "It returned a string" is not "the room saw it".
- **Row 7** is the third-party half. Our fixture agent is, unavoidably, an agent
  written against the same reading of the SDK as the client. A real agent is the
  only thing that can falsify that reading. Hermes additionally needs an LLM
  backend configured, which makes it a slower and less deterministic test — worth
  doing once as a receipt, not worth putting in CI.

### A trap worth knowing before you write an ACP agent

`on_receive_request` callbacks hold the connection's dispatch loop until they
return. Awaiting a nested `send_request(..).block_task()` *inside* one therefore
deadlocks by construction — the reply can only be delivered by the loop you are
holding. The SDK documents this under "Deadlock Risk"; the first version of our
test fixture did it anyway and hung for twenty minutes producing no output at
all, which is why every test in this crate now runs under an explicit deadline.
A hung test is strictly worse than a failing one: it looks like slowness.

The bridge's own permission handler is not exposed to this — it decides purely
and responds without awaiting anything — but any agent you write is.
