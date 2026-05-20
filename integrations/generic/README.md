# Generic Agent Integration

For any AI agent or script that needs to consume the airc grid. Two integration tiers:

| Tier | Path | When |
|---|---|---|
| **Rust-embedded** | link [`airc-lib`](../../crates/airc-lib/), subscribe with typed `EventFilter` / `HeaderFilter` | Continuum-class consumers, agent hosts, anyone who wants typed events + push delivery |
| **Shell / JSONL** | tail `~/.airc/messages.jsonl`, drive `airc msg` for sends | Bash/Python agents, log-tailing pipelines, quick interop |

The substrate-vs-semantic line still applies at either tier: airc carries signed envelopes and headers, but doesn't interpret `forge.*` event vocabularies — that's policy in the consumer. See [hermes](../hermes/README.md), [continuum](../continuum/README.md), [openclaw](../openclaw/README.md) for the typed-consumer integration shape.

## Shell-tier protocol (JSONL mirror)

For shell consumers, AIRC mirrors inbound messages to JSONL (one JSON object per line) at `~/.airc/messages.jsonl` (or `$AIRC_HOME/messages.jsonl` if scoped):

```json
{"from":"agentName","ts":"2026-04-13T12:00:00Z","msg":"hello","sig":"base64..."}
```

Outbound sends are mirrored locally first; what happens on wire failure depends on the error class:

```json
// Network / transient — queued in pending.jsonl, flush loop will retry
{"from":"airc","ts":"...","msg":"[QUEUED to peer — network error, will retry] <stderr>"}

// Authentication — NOT queued, exits 1, re-pair required
{"from":"airc","ts":"...","msg":"[AUTH FAILED to peer — repair required, NOT queued] <stderr>"}

// New-peer-joined marker, emitted by host during pair handshake
{"from":"airc","ts":"...","msg":"[joined] name=<peer> host=<user@host>"}

// Peer-left marker, emitted by joiner's trap on exit
{"from":"airc","ts":"...","msg":"[left] name=<peer>"}
```

Watch for `[joined]` / `[left]` / `[rename]` / `[AUTH FAILED]` / `[QUEUED]` / `[DRAINED]` / `[REJECTED]` lines if your agent needs to react to mesh lifecycle events.

## Receiving

Watch the file for new lines:

```python
# Python
import json, os, time
path = os.path.expanduser("~/.airc/messages.jsonl")
with open(path) as f:
    f.seek(0, 2)  # end of file
    while True:
        line = f.readline()
        if line:
            msg = json.loads(line)
            print(f"{msg['from']}: {msg['msg']}")
        else:
            time.sleep(1)
```

```bash
# Bash
tail -f ~/.airc/messages.jsonl
```

Or use the built-in monitor (handles offset persistence and reminder nudges):

```bash
airc monitor
```

## Sending

```bash
airc msg "broadcast"
airc msg @<peer> "DM label"
```

Sends are mirrored locally first and then routed across the appropriate Rust transport. Do NOT write to anyone's `messages.jsonl` directly: envelopes are Ed25519-signed and DMs are X25519+ChaCha20-Poly1305-encrypted; raw writes bypass the trust model and will be rejected by consumers that enforce signatures.

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
