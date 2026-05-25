# Continuum Integration

Continuum is the capability host in the airc grid model — it owns LLMs, LoRA collections, paging strategy, and persona state. Continuum personas run as airc peers. The integration shape is a typed event vocabulary on the airc wire, not a direct API binding.

The implementation direction is Rust-first. Continuum should use AIRC
as its chat/event/work substrate and keep runtime-critical state in
Rust-backed services and typed AIRC projections. TypeScript should
shrink toward generated bindings, thin UI presentation, and browser
integration instead of owning duplicate chat buses, event replay,
room membership, or distributed inference coordination.

<img src="https://raw.githubusercontent.com/CambrianTech/continuum/main/docs/images/live-session-avatars.png" alt="Continuum live room with one human and AI personas represented as avatars in a shared video conversation" width="100%"/>

In a live room, Continuum can use AIRC channels for room presence,
chat, persona turns, command events, WebRTC signaling, and
DataChannel control. Audio/video media rides the WebRTC media path;
AIRC coordinates the signed channel and peer lifecycle around it.
That same substrate shape can also serve OpenClaw chat surfaces,
Hermes orchestration, Slack-like bridges, and other consumers without
making AIRC specific to any one product.

## How Continuum embeds airc

Continuum links [`airc-lib`](../../crates/airc-lib/) directly. The embedding shape is the one proven by [`embedded_consumer_smoke`](../../crates/examples/embedded_consumer_smoke/) and codified in [`consumer_shapes::continuum`](../../crates/examples/consumer_shapes/src/continuum.rs):

```rust
use airc_lib::{Airc, Body, EventFilter, HeaderFilter, Headers};
use consumer_shapes::continuum::{
    encode_persona_event, PersonaEvent, TurnEmitted, activity_event_filter,
};

let airc = Airc::open("~/.airc").await?;
airc.join_with_wire("project-room", wire_path).await?;

// Persona emits a turn output
let event = PersonaEvent::TurnEmitted(TurnEmitted {
    persona_id: "skylar".into(),
    activity_id: "session-42".into(),
    turn_id: "turn-1".into(),
    text: "ship the substrate".into(),
    emitted_at_ms: now_ms(),
});
let (headers, body) = encode_persona_event(&event)?;
airc.send(body, headers).await?;

// Another component subscribes to one activity, cursor-aware on restart
let mut stream = airc.subscribe_filtered(activity_event_filter("session-42")).await?;
```

## Wire contract

Body hint: `forge.persona.event.v1`.

Projected headers (subscribers filter on these without parsing the body):

| Header | Meaning |
|---|---|
| `forge.persona.kind` | event variant: `turn_requested` / `turn_emitted` / `activity_started` / `activity_ended` |
| `forge.persona.id` | persona that emitted the event |
| `forge.continuum.activity_id` | scoping activity |
| `forge.continuum.turn_id` | per-turn id (turn events only) |

Real Continuum extends `PersonaEvent` with more variants (RAG fetches, memory writes, etc.) following the same shape — typed noun struct + variant in the enum + header projection.

## Substrate-vs-policy line

The substrate routes events whose headers match a filter. **It does not know what `forge.persona.*` means.** Capability projection (which persona handles which activity, which Continuum host has which LoRA loaded) lives in Continuum, never in airc. If airc started interpreting `forge.persona.kind`, the layer would dissolve.

## See also

- Continuum repo: [`docs/architecture/AGENT-BACKBONE-INTEGRATION.md`](https://github.com/CambrianTech/continuum/blob/canary/docs/architecture/AGENT-BACKBONE-INTEGRATION.md) — the integration story from Continuum's side, with the 3-layer architecture and Codex's substrate-vs-semantic correction
- [`crates/examples/consumer_shapes/src/continuum.rs`](../../crates/examples/consumer_shapes/src/continuum.rs) — typed `PersonaEvent` + codec
- [`crates/examples/consumer_shapes/tests/continuum_roundtrip.rs`](../../crates/examples/consumer_shapes/tests/continuum_roundtrip.rs) — fixture tests proving codec + filter behavior
