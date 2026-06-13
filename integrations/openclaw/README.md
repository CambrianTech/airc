# OpenClaw Integration

OpenClaw has its own user identity model and thread/workspace taxonomy that predates airc. The integration shape is a typed adapter that carries OpenClaw identifiers alongside the airc envelope, so consumers can route events by either system's notion of "who" and "where":

- OpenClaw user → airc `PeerId` (substrate identity), plus `forge.openclaw.user_id` retained as a header for OpenClaw-aware subscribers
- OpenClaw thread → airc `RoomId` (substrate channel), plus `forge.openclaw.thread_id` retained
- OpenClaw workspace → projected as a header for routing; no substrate concept needed

## How OpenClaw embeds airc

OpenClaw links [`airc-lib`](../../crates/airc-lib/). The integration shape is codified in [`consumer_shapes::openclaw`](../../crates/examples/consumer_shapes/src/openclaw.rs):

```rust
use airc_lib::{Airc, Body, EventFilter, HeaderFilter, Headers};
use consumer_shapes::openclaw::{
    encode_openclaw_event, OpenClawEvent, ChatMessagePosted, workspace_event_filter,
};

let airc = Airc::open("~/.airc").await?;

let event = OpenClawEvent::ChatMessagePosted(ChatMessagePosted {
    openclaw_user_id: "u-alice".into(),
    openclaw_thread_id: "t-router-debug".into(),
    openclaw_workspace_id: "w-acme".into(),
    text: "PR-G CI is green".into(),
    posted_at_ms: now_ms(),
});
let (headers, body) = encode_openclaw_event(&event)?;
airc.send(body, headers).await?;

// Cross-thread routing: subscribe to one workspace
let mut stream = airc.subscribe_filtered(workspace_event_filter("w-acme")).await?;
```

## Wire contract

Body hint: `forge.openclaw.event.v1`.

Projected headers:

| Header | Meaning |
|---|---|
| `forge.openclaw.kind` | `chat_message_posted` / `thread_created` |
| `forge.openclaw.user_id` | OpenClaw user (stable across thread changes) |
| `forge.openclaw.thread_id` | OpenClaw thread (maps to AIRC channel) |
| `forge.openclaw.workspace_id` | OpenClaw workspace (cross-thread routing key) |

Real OpenClaw extends with more variants (mentions, reactions, edits, attachments) following the same shape.

## See also

- [`crates/examples/consumer_shapes/src/openclaw.rs`](../../crates/examples/consumer_shapes/src/openclaw.rs) — typed `OpenClawEvent` + codec
- [`crates/examples/consumer_shapes/tests/openclaw_roundtrip.rs`](../../crates/examples/consumer_shapes/tests/openclaw_roundtrip.rs) — fixture tests including workspace-scope filter behavior
