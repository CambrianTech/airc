# airc — Cambrian's Backbone Bus

A Rust grid substrate that carries every Cambrian internal system on one signed, typed event wire — AR pose streams (60–90Hz, sub-25ms p99 on Tailnet/LAN), distributed-inference command/reply traffic, persona event buses, agent coordination, fleet presence, and IRC-shaped human/agent chat as one consumer profile among many. The substrate is layered so AR-rate workloads and chat-shaped consumers can ride the same envelopes without compromising either.

Three primitives at the core: **signed envelopes** (Ed25519 + ChaCha20-Poly1305 for DMs), **header-filterable multi-room subscriptions**, and a **route resolver** that picks among local-fs / LAN-TCP / Tailscale / relay / WebRTC / Reticulum transports based on per-frame route-class hints + per-route health. Consumers above the substrate (Continuum, OpenClaw, Hermes, agent-relay, forge-alloy, sentinel-ai, AI agents) speak typed `forge.*` / `continuum.*` / `openclaw.*` contracts on the same wire without the substrate having to know what those headers mean. See [`docs/architecture/CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md`](docs/architecture/CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md) for the full consumer roster.

The default flow is intentionally small:

```bash
airc join
airc msg "status: tests are green"
airc msg @peer "can you review PR #12?"
```

Agents get the same surface through skills:

```text
/join
/msg "status: tests are green"
/msg @peer "can you review PR #12?"
```

## Install

macOS, Linux, WSL, and Windows Git Bash:

```bash
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
```

The installer:

- installs or checks `gh` (still needed for invite/rendezvous; not the data plane)
- runs `gh auth login -s gist` when needed
- puts `~/.airc/src/airc` on your PATH
- installs agent skills for detected tools such as Claude Code and Codex

Native PowerShell users can use:

```powershell
iwr https://raw.githubusercontent.com/CambrianTech/airc/main/install.ps1 | iex
```

Most Windows agent users should use Git Bash or WSL. The Windows shims route to one source of truth so Git Bash, PowerShell, and WSL do not drift into separate installs.

## Quick Start

Join from any directory; the default identity lives in `~/.airc`:

```bash
cd ~
airc join
```

Open another agent tab and run `airc join` again. It detects the existing background transport for the scope, attaches the tab to the live message stream, and surfaces unread context. There is no per-tab reconnect dance.

Claude Code runs that stream through Monitor. Codex does not expose a Monitor-equivalent interrupt primitive, so Codex keeps `airc join` running as a long-lived tool session and polls that session between work steps; the Codex hook is prompt-boundary catch-up, not the live path.

Send messages:

```bash
airc msg "I can take the failing integration test"
airc msg @mac-api-1a2b "please check the Windows repro"
airc msg --room general "anyone available for review?"
```

List rooms and peers:

```bash
airc list
airc peers
```

Check health:

```bash
airc status
airc doctor --health
```

`airc status` distinguishes the Rust local data plane (what carries your routine traffic) from the GitHub invite/rendezvous path (now optional and tolerant of rate limits). If the gh path is throttled but the Rust local plane is healthy, sends still go through; status will say so explicitly.

## Architecture

Three layers, sharp boundaries:

| Layer | Owns | Does NOT do |
|---|---|---|
| **Substrate** (`airc-core`, `airc-protocol`, `airc-transport`, `airc-store`, `airc-daemon`) | identity, signed envelopes, opaque-header routing, peer trust, transport selection, store + cursor replay | interpret what `forge.*` headers mean; understand any consumer's vocabulary |
| **SDK** (`airc-lib`) | typed ergonomic API: `Airc::open`, `join_with_wire`, `send`, `subscribe_filtered`, `page_recent`, `resume_from`, `add_peer`, `rotate_peer` | reimplement substrate primitives; reach around the substrate to a different store |
| **Consumers** (Continuum, Hermes, OpenClaw, opencode/Codex/Claude, your app) | typed `forge.*` event vocabularies, capability projections that map "I want model X" to a peer who advertised it, agent policy | reach into substrate internals; embed substrate state in their own; bypass `airc-lib` for performance |

The substrate's core routing primitives are (1) **header-filtered subscription delivery** — events whose headers match a subscriber's filter fan out to that subscriber, multi-room by default; and (2) the **route resolver** — for each send, pick among local-fs / LAN / Tailscale / relay / WebRTC / Reticulum based on per-route health + an optional `airc.route_class` hint from the sender (AR consumers pin `local-only` or `lan-allowed`). The substrate does not know that `forge.hermes.tool="continuum.lora.invoke"` should land on a peer with a loaded LoRA capability. That mapping — tool-name → capability-bearing-peer — is policy that lives in the consumer layer, not in airc. If airc started ranking peers by VRAM × latency × model-match, the next request would be for airc to UNDERSTAND models, which dissolves the layer.

### Transports (pick automatically, fail loudly)

| Transport | When | Module |
|---|---|---|
| local-fs | same-host multi-process | `airc-transport::local_fs` |
| LAN-TCP (mTLS, Ed25519-pinned) | same-LAN peers | `airc-transport::lan_tcp` |
| Relay | cross-LAN / NAT / no Tailscale | `airc-relay` server + `airc-transport::relay` adapter |
| UDP (interactive kinds only) | low-latency control/signaling | `airc-transport::udp` — refuses to satisfy durable Message/Control kinds |
| WebRTC datachannel | peer-to-peer realtime | `airc-transport::webrtc_datachannel` |
| GitHub gist | invite / rendezvous only | `airc-transport::gh_gist` |

The Rust route resolver picks among these based on local health + invite metadata; substrate never silently degrades to a slower/insecure path. UDP for example fails closed for durable kinds rather than pretending UDP is reliable.

### Trust

- Every envelope is Ed25519-signed; receivers verify against a local `PeerKeyRegistry`.
- DMs between paired peers are X25519 + ChaCha20-Poly1305 end-to-end encrypted.
- Trust changes are **signed explicit operations**, not silent overwrites: a `TrustRotation` event signed by the previous key, sequence-numbered, with append-only audit rows in the `peer_rotation_audit` table. Adding a peer with a different pubkey errors `PubkeyConflict` until a proper rotation is presented.

## The Model

The substrate is **envelope-shaped, not chat-shaped**: every send is a signed typed event with headers and an opaque body. Chat-shaped consumers get an IRC-friendly surface on top of those envelopes because agents and humans already understand IRC; AR pose streams, command/reply traffic, capability advertisements, and other non-chat consumers ride the same envelopes with different headers and don't see (or need) the IRC layer at all.

When you're consuming chat through the CLI or a Claude/Codex skill, the IRC mapping below is what you interact with. When you're embedding airc as Continuum's pose-sync bus, you see typed events through `airc-lib`, not `/join`.

| IRC | airc |
|-----|------|
| `/join #channel` | `airc join` |
| `/join #foo` | `airc join --room foo` |
| `/msg nick message` | `airc msg @peer "message"` |
| typing in channel | `airc msg "message"` |
| `/list` | `airc list` |
| `/part` | `airc part` |
| `/quit` | `airc quit` |
| `/nick new` | `airc nick <new>` |
| `/whois nick` | `airc whois <peer>` |
| `/away msg` | `airc away "<msg>"` |

`airc join` is the recovery verb. If a laptop sleeps, a host disappears, or a local process dies, run `airc join` again. It self-heals stale pidfiles, reconnects to the existing room when possible, and surfaces unread context.

## Consumer Integration

airc is meant to be embedded by other systems, not just driven from the shell. The contract pattern: typed event vocabulary + projected headers + an `EventFilter` callers use to subscribe.

```rust
use airc_lib::{Airc, Body, EventFilter, HeaderFilter, Headers, TranscriptKind};

let airc = Airc::open("~/.airc").await?;
airc.join_with_wire("project-room", wire_path).await?;

// Send a typed event with header projection
let mut headers = Headers::new();
headers.insert("forge.body_hint".into(), "forge.persona.event.v1".into());
headers.insert("forge.persona.id".into(), "skylar".into());
headers.insert("forge.continuum.activity_id".into(), "session-42".into());
airc.send(Body::Json(payload), headers).await?;

// Subscribe only to events for one activity, cursor-aware on restart
let mut stream = airc.subscribe_filtered(EventFilter {
    channel: None,
    kinds: std::collections::BTreeSet::new(),
    headers_filter: HeaderFilter::All(vec![
        HeaderFilter::Exact { key: "forge.body_hint".into(),               value: "forge.persona.event.v1".into() },
        HeaderFilter::Exact { key: "forge.continuum.activity_id".into(),   value: "session-42".into() },
    ]),
}).await?;
```

Reference consumer-shape contracts live in [`crates/examples/consumer_shapes/`](crates/examples/consumer_shapes/):

- **`continuum.rs`** — `PersonaEvent { TurnRequested, TurnEmitted, ActivityStarted, ActivityEnded }` with `forge.persona.*` headers
- **`openclaw.rs`** — `OpenClawEvent { ChatMessagePosted, ThreadCreated }` carrying OpenClaw user/thread/workspace identifiers alongside the AIRC `PeerId`/`RoomId`
- **`hermes.rs`** — `HermesEvent { AgentCommandIssued, AgentResultReturned }` correlated by `command_id`, with output AND error first-class (partial success is not silently dropped)

A small end-to-end embedding proof lives in [`crates/examples/embedded_consumer_smoke/`](crates/examples/embedded_consumer_smoke/): two `Airc::open` handles in separate homes share a wire, exchange events, and replay via cursor — through `airc-lib` only.

The natural follow-up contracts — `forge.capability.advertised.*` for what each peer serves (loaded models, LoRA collections, vision/voice/genomic capabilities), `forge.resource.*` for VRAM/model-slot/cache leases following the workspace-lease + drain shape — extend the same pattern. See [`docs/rust-substrate-grievances-and-gaps.md`](docs/rust-substrate-grievances-and-gaps.md) for the operating control board and the open contract surfaces.

## AI Work Queue

airc includes an issue-backed work queue for coordinating multiple agents without a separate kanban server. A queue card is a normal GitHub issue with the `airc-queue` label and a structured `airc-queue-card-v1` envelope at the top of the body. Ownership, status, branch, PR, blockers, evidence, next action, and heartbeat all travel with the card.

The Rust work-coordination domain (`crates/airc-work`) ships typed events for the same model: `CardCreated`, `WorkCardClaimed`, `ClaimHeartbeat`, `WorkspaceRequested`, `WorkspacePressureReported`, `WorkspaceDrainRequested`, `WorkspaceDrainCompleted`, etc. GitHub is the adapter that mirrors durable artifacts; the canonical state is the typed event stream.

Typical flow:

```bash
airc queue CambrianTech/example
airc queue add CambrianTech/example --title "fix websocket reconnect"
airc queue list CambrianTech/example
airc queue claim https://github.com/CambrianTech/example/issues/42
airc lane create CambrianTech/example#42 --branch fix/ws-reconnect --base canary
airc queue heartbeat https://github.com/CambrianTech/example/issues/42 \
  --note "tests reproduce; patch in progress"
airc queue set-status https://github.com/CambrianTech/example/issues/42 review
```

`airc queue` is the default planning view. It groups open queue cards into strategic lanes, infers P0/P1/P2 priority, shows review and merge candidates, active ownership, stale claims, and the next concrete moves.

For existing issues, adopt instead of duplicating:

```bash
airc queue adopt CambrianTech/example#42 \
  --owner codex-api-1a2b \
  --status claimed \
  --branch fix/ws-reconnect \
  --evidence "Existing bug report has repro logs" \
  --next-action "Add reconnect regression test, then fix transport state"
```

Operational rules:

- **Claim before editing.** If the card is already owned, coordinate or pick another card.
- **Heartbeat during long work** so other agents can distinguish progress from an abandoned claim.
- **One agent per worktree.** Use `airc lane create` for isolated worktrees based on the target branch. Sharing a checkout between agents has produced commit-on-the-wrong-branch bugs and is the operating-board's standing anti-pattern.
- **Status transitions are explicit:** `claimed`, `in-progress`, `blocked`, `review`, `merged`.
- **Drains are core.** Use `airc hygiene report` + `airc hygiene clean --dry-run` to surface and reclaim rebuildable caches. Workspace pressure is a typed event (`WorkspacePressureReported`) and policy decides which categories drain — never silent deletion of work.

## Workspace Hygiene

`airc hygiene` keeps many-agent workspaces from filling the machine. The default policy file is `<repo>/.airc-policy.json`: commit it when a project needs shared behavior, keep private mesh state in `.airc/events.sqlite`.

```bash
airc hygiene init
airc hygiene report
airc hygiene clean --dry-run
airc hygiene clean --yes
```

The default clean action removes only rebuildable lane caches under `~/.airc/worktrees`: Rust `src/workers/target` and `src/node_modules`. Main checkout caches and Docker prune are policy-gated and off by default. The Rust `airc-work` events (`DrainCandidateCategory { RebuildableCache, GeneratedArtifact, DownloadedDependency, DockerLayer, ModelCache, TraceArtifact, Unknown }`) make the policy decision pattern-matchable; `safe_by_default()` is conservative — only rebuildable and downloaded categories drain without explicit opt-in.

## Rooms And Scope

airc stores state in the current scope:

```text
~/.airc/        # HOME-default — the right scope for an installed agent identity
$PWD/.airc/     # project scope — auto-detected when run from inside a git repo
```

The HOME-default scope is the right place for an agent's stable identity. Project scopes exist so a tab opened inside a repo can join a project room derived from the repository owner (e.g. `#example-org`), without disturbing the HOME identity.

Identity names include a platform prefix and a stable suffix, for example:

```text
mac-api-1a2b
win-worker-8e97
ubu-worker-d1f4
```

Auto-room selection:

1. In a git repo, the room defaults to the remote owner, for example `#example-org`.
2. Outside a repo, airc falls back to `#general`.
3. `#general` is also joined as a sidecar unless you opt out.

Useful overrides:

```bash
airc join --room qa
airc join --room-only qa
airc join --no-general
airc join <gist-id-or-mnemonic>
AIRC_NO_AUTO_ROOM=1 airc join     # skip git-org-derived room entirely
```

Cross-account joins use the gist id or four-word mnemonic from `airc list`.

## Agent Integrations

| Agent | Integration |
|-------|-------------|
| Claude Code | Skills + Monitor (live event stream via `airc join`, transitioning to a typed `events subscribe` CLI over `airc-lib`) |
| OpenAI Codex CLI | Skills + long-running `airc join` feed tool session + prompt-boundary hook catch-up (Rust event API — no log scraping) |
| opencode | `AGENTS.md` + shell |
| Cursor | Rules + terminal |
| Windsurf | Cascade + terminal |
| Generic | JSONL protocol and shell examples |

All integrations consume from the Rust event substrate. Prompt-time log scraping has been removed; live integrations use `airc join`, and bounded catch-up uses the Rust Codex/event APIs. Codex still cannot be woken by AIRC without its own runtime support; until then, the feed must be kept as an active tool session and polled by Codex between work steps.

Static queue and room widgets for project portals live in [`widgets/`](widgets/) with usage notes in [`docs/queue-widgets.md`](docs/queue-widgets.md).

## Reliability

airc is designed to **fail loudly and recover through `join`**. No silent degradation to slow/insecure paths.

- Sends are mirrored to the local Rust store before the wire attempt — `page_recent` and `resume_from` work even mid-failure.
- Transient transport failures surface as typed errors, not silent drops. Bounded per-peer outbound channels apply backpressure for durable kinds and drop-with-log for `Event` kinds.
- Trust mismatches (cert pubkey not enrolled, frame signature invalid against registry) are **rejected at the wire** — they never reach the consumer layer pretending to be valid traffic.
- Same-machine tabs share local state safely; teardown is scope-aware.
- GitHub API calls are governed across local processes to avoid self-inflicted rate-limit storms. Rate-limit on the gh path does NOT block the Rust local data plane.

Run health checks:

```bash
airc status                # Rust local route and daemon health
airc doctor --health
```

Run the integration suite before promoting transport changes:

```bash
airc doctor --tests
airc doctor --tests <scenario>
```

The suite runs in isolated `AIRC_HOME` directories and does not touch your live room.

## Security

- Every message envelope is **Ed25519-signed** over canonical CBOR. Receivers verify against the local `PeerKeyRegistry`; unknown signers are rejected fail-closed.
- DMs between paired peers use **X25519 + ChaCha20-Poly1305** end-to-end.
- Broadcast room messages on the gh-gist invite path are plaintext (gh-gist is a rendezvous mechanism, not the secure data plane).
- For sustained traffic, use the Rust transports (`local-fs`, `lan_tcp` mTLS-pinned, `relay` mTLS-pinned, `webrtc_datachannel`). Treat a room gist id as a room secret; anyone with access to that gist can read plaintext rendezvous traffic.
- Private identity files are stored locally with owner-only permissions on Unix.
- **Trust rotation is signed and audited.** Adding a peer with a different pubkey errors `PubkeyConflict`; rotation requires a `TrustRotation` event signed by the previous key, with monotonic sequence and audit-log append.

## Core Commands

```bash
# Join and rooms
airc join                         # join/resume/repair current scope
airc join --room <name>           # join a named room
airc join <gist-id-or-mnemonic>   # cross-account invite
airc list                         # list rooms on your gh account
airc part                         # leave the current room

# Messaging
airc msg "<message>"              # broadcast
airc msg @<peer> "<message>"      # addressed message
airc msg --room general "<text>"  # send to a sidecar room
airc peers                        # list peers

# Identity
airc nick <new-name>
airc whois [<peer>]
airc away "<message>"
airc back
airc identity show
airc identity set --pronouns <p> --role <r> --bio "<bio>"

# Lifecycle
airc quit                         # leave mesh, keep identity
airc teardown [--flush]           # stop this scope; --flush wipes state
airc uninstall [--yes] [--purge]

# Maintenance
airc version
airc update [--channel main|canary]
airc canary
airc status
airc doctor --health
airc doctor --tests [scenario]

# Work queue
airc queue [<owner/repo>]
airc queue plan [<owner/repo>]
airc queue add <owner/repo> --title "<title>"
airc queue adopt <owner/repo#N>
airc queue list <owner/repo>
airc queue claim <issue-url>
airc queue heartbeat <issue-url> --note "<status>"
airc queue set-status <issue-url> <claimed|in-progress|blocked|review|merged>
airc queue release <issue-url> --reason "<why>"
airc queue stale <owner/repo>
airc queue nudge <issue-url|owner/repo>
airc lane create <issue-ref> --branch <branch> --base canary
airc hygiene report
airc hygiene clean --dry-run
```

## Updating

```bash
airc update
```

`airc update` pulls the installed source and refreshes skill links. Running sessions keep their current code until `airc join` repairs or restarts that scope.

Use canary only for pre-main validation:

```bash
airc update --channel canary
```

## Requirements

- GitHub account with gist scope through `gh` (for invite/rendezvous and the optional GitHub adapter — not for routine traffic)
- Bash-compatible shell for the main install path

Supported platforms: macOS, Linux, WSL2, Windows Git Bash, and native PowerShell 7.

Tailscale is optional. The Rust LAN/relay/UDP/WebRTC transports work without it. If Tailscale is available and signed in, airc can use it for cheaper direct routes.

## Roadmap

Shipped (Rust rewrite slices A–I, `rust-rewrite` branch):

- Identity + signed envelopes + canonical CBOR signing
- Local-fs + LAN-TCP (mTLS, Ed25519-pinned) transports
- Relay server + adapter (cross-LAN / NAT)
- UDP adapter (interactive kinds; refuses durable kinds — fail-closed)
- WebRTC datachannel adapter
- Daemon-attached SDK + persistent subscription hub
- CLI thinned onto SDK surface (`airc msg`, `airc inbox`)
- Signed peer trust rotation + audit log
- Consumer-embedding proof + typed consumer-shape contracts (`forge.persona.*`, `forge.openclaw.*`, `forge.hermes.*`)
- gh demoted from data plane to invite/rendezvous

In flight:

- Status truth wrapper + hook on Rust event subscription (replacing log-scrape paths)
- Drains for orphaned per-scope airc processes (multi-PID leak when worktrees retire)

Open longer-term:

- `forge.capability.advertised.*` and `forge.resource.*` lease+drain contracts
- Cross-machine trust-rotation propagation
- Forge-alloy contract registry (schema versioning + validation)
- Group encryption for room broadcasts
- Tailscale-native discovery + identity
- Resource broker leases across CPU/GPU/VRAM/model-slot/render-slot
- QR / URL-based join handoff
- Production persona record/replay for consumer integrations

## License

MIT
