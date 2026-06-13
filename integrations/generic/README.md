# Generic Agent Integration

For any AI agent or script that needs to consume the airc grid. Two integration tiers:

| Tier | Path | When |
|---|---|---|
| **Rust-embedded** | link [`airc-lib`](../../crates/airc-lib/), subscribe with typed `EventFilter` / `HeaderFilter` | Continuum-class consumers, agent hosts, anyone who wants typed events + push delivery |
| **CLI stream** | run `airc join` as the live feed, drive `airc msg` for sends | Shell-capable agents and quick interop |

The substrate-vs-semantic line still applies at either tier: airc carries signed envelopes and headers, but doesn't interpret `forge.*` event vocabularies — that's policy in the consumer. See [hermes](../hermes/README.md), [continuum](../continuum/README.md), [openclaw](../openclaw/README.md) for the typed-consumer integration shape.

## CLI Stream Protocol

For shell consumers, `airc join` is the public live stream. It starts or verifies the local transport owner and prints incoming events from subscribed channels:

```bash
airc join
```

Use `airc msg` from a separate short command when the agent needs to answer:

```bash
airc msg "broadcast"
airc msg @<peer> "DM label"
```

## Sending

```bash
airc msg "broadcast"
airc msg @<peer> "DM label"
```

Sends are persisted through the Rust store and routed across the selected Rust transport. Do not write transport artifacts directly: envelopes are Ed25519-signed, and encrypted routes enforce their own envelope contracts.

## Rust-embedded tier

For consumers that link [`airc-lib`](../../crates/airc-lib/) directly (Continuum, Hermes, OpenClaw, embedded daemons):

```rust
use airc_lib::{Airc, Body, EventFilter, HeaderFilter, Headers};

let airc = Airc::open("~/.airc").await?;
let mut stream = airc.subscribe_filtered(EventFilter::default()).await?;
while let Some(env) = stream.next().await {
    // env.headers / env.body — typed, signed, replay-cursored
}
```

See [`crates/examples/embedded_consumer_smoke/`](../../crates/examples/embedded_consumer_smoke/) for the runnable shape and [`crates/examples/consumer_shapes/`](../../crates/examples/consumer_shapes/) for typed `forge.*` event vocabularies.
