# airc as an ACP client — one bridge, N agents

**Status:** design pinned against the real SDK (2026-08-10). Implementation next.

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

1. `uvx hermes-agent[acp]` spawned from airc on Windows;
2. a message posted in a room reaches the agent as a `PromptRequest`;
3. its reply is published back into the room and visible to a *second* peer;
4. a tool-permission request with no `toolsAllow` entry is denied AND the denial
   is visible in the room;
5. killing the agent process produces a room-visible failure, not silence.
