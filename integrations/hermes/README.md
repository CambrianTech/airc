# Hermes Integration

Hermes is the agent-tool orchestrator in the airc grid model — agents issue commands targeting tools by name, tool invocations run on whichever peer has the capability, results return correlated by `command_id`. Hermes doesn't pick which machine runs a tool; the capability projection above the substrate does.

## How Hermes embeds airc

Hermes links [`airc-lib`](../../crates/airc-lib/). The integration shape is codified in [`consumer_shapes::hermes`](../../crates/examples/consumer_shapes/src/hermes.rs):

```rust
use airc_lib::{Airc, Body, EventFilter, HeaderFilter, Headers};
use consumer_shapes::hermes::{
    encode_hermes_event, HermesEvent, AgentCommandIssued, AgentResultReturned,
    agent_event_filter,
};

let airc = Airc::open("~/.airc").await?;

// Agent issues a tool invocation; substrate routes by header match
let issue = HermesEvent::AgentCommandIssued(AgentCommandIssued {
    agent_id: "agent-orion".into(),
    command_id: "cmd-001".into(),
    tool: "continuum.lora.invoke".into(),
    input: serde_json::json!({ "adapter_id": "code-review-v3", "prompt": "..." }),
    issued_at_ms: now_ms(),
});
let (headers, body) = encode_hermes_event(&issue)?;
airc.send(body, headers).await?;

// Orchestrator subscribes to one agent's full command lifecycle
let mut stream = airc.subscribe_filtered(agent_event_filter("agent-orion")).await?;
```

## Wire contract

Body hint: `forge.hermes.event.v1`.

Projected headers:

| Header | Meaning |
|---|---|
| `forge.hermes.kind` | `agent_command_issued` / `agent_result_returned` |
| `forge.hermes.agent_id` | issuing or receiving agent |
| `forge.hermes.command_id` | correlates command → result |
| `forge.hermes.tool` | tool / capability name |

## Partial-success is first-class

`AgentResultReturned` carries BOTH `output: Option<Value>` AND `error: Option<String>` — a tool that produced half its output and then hit an error must serialize both. *"It worked"* without specifics is not acceptable.

```rust
HermesEvent::AgentResultReturned(AgentResultReturned {
    agent_id: "agent-orion".into(),
    command_id: "cmd-002".into(),
    tool: "fs.read".into(),
    output: Some(json!({ "content": "partial-data" })),
    error: Some("EOF before complete read".into()),
    returned_at_ms: now_ms(),
})
```

## Capability routing lives ABOVE the substrate

The substrate routes events whose headers match a filter. It does NOT know that `forge.hermes.tool="continuum.lora.invoke"` should land on a Continuum host with that LoRA loaded. That mapping — tool-name → capability-bearing-peer — is policy in Hermes (or a peer-table projection Hermes maintains by subscribing to `forge.capability.advertised.*` events). The substrate carries the events; Hermes decides which peer to invoke.

## See also

- [`crates/examples/consumer_shapes/src/hermes.rs`](../../crates/examples/consumer_shapes/src/hermes.rs) — typed `HermesEvent` + codec
- [`crates/examples/consumer_shapes/tests/hermes_roundtrip.rs`](../../crates/examples/consumer_shapes/tests/hermes_roundtrip.rs) — fixture tests including the partial-success roundtrip
- [Continuum integration](../continuum/README.md) — how `forge.persona.*` interlocks with `forge.hermes.*` for cross-system orchestration
