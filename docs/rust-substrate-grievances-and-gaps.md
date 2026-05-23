# AIRC Rust Substrate Grievances And Gaps

**Status:** audit record  
**Date:** 2026-05-19  
**Scope:** AIRC Rust rewrite, current CLI/daemon/transport stack, queue/lane/workspace coordination, and consumer integration for Continuum, OpenClaw, Hermes, opencode, and future grids.

## Branch Truth

The latest small PR is not the strategic unit of work. The strategic unit is the Rust rewrite consolidation branch.

In the current checkout the branch is named `rust-rewrite`. If the intended canonical name is `rust-reworks`, rename it once and make that the only long-lived Rust substrate branch.

Policy:

- `rust-rewrite` / `rust-reworks` is the Rust substrate integration branch.
- All already-open Rust substrate feature branches must merge into it or be closed.
- No new long-lived `feat/airc-*` branch should survive after its PR lands.
- Canary receives coherent Rust substrate promotion from the integration branch.
- Main receives canary after full cross-platform proof.
- Branches are temporary review packets, not memory storage.
- Work not merged into the integration branch is not done.

Current branch sprawl itself is a grievance. Multiple historical branches contain partial Rust substrate work: protocol, headers, identity, local-fs, LAN-TCP, signed transport, CLI, daemon, room defaults, blobs, and fixes. That is not a stable operating model. The system needs one integration truth.

## Operating Control Board

This document is not advisory. Until the Rust substrate is coherent, it is the control board for AIRC architecture work.

### Active Canonical Branch

- Canonical integration branch: `rust-rewrite` unless explicitly renamed once to `rust-reworks`.
- Feature branches: short-lived review packets only.
- Canary: promotion target after coherent Rust integration, not the place where Rust substrate fragments accumulate.
- Main: release target after canary and cross-platform proof.

### Work Intake Rule

Every new AIRC Rust task must name:

- which grievance number it closes;
- which gap list item it closes;
- which acceptance gate it advances;
- whether it removes Python/shell, leaves it untouched, or temporarily bridges it;
- which branch it targets;
- which test proves it.

If a task cannot answer those fields, it is not ready to code.

### Stop-Doing List

Do not:

- add runtime behavior to Python;
- add runtime behavior to shell;
- create another long-lived Rust feature branch;
- target canary with Rust substrate fragments;
- use GitHub as the routine same-machine or Tailscale message bus;
- claim Windows support from install-only checks;
- ship a fallback path that silently degrades to slow/insecure behavior;
- add consumer-specific semantics to AIRC core;
- make CLI prose the integration API;
- leave an open PR as the final state of work.

### First Remediation Slices

The next slices should be boring and sequential:

1. **Branch stabilization:** confirm `rust-rewrite` is the integration truth, close/retire stale Rust branches, and open future PRs against `rust-rewrite`.
2. **Windows IPC correctness:** fix named-pipe uniqueness, add pipe-name uniqueness tests, and add Windows runtime IPC smoke.
3. **Transport send correctness:** prevent post-accept silent LAN-TCP drops by checking/lifting frame size before enqueue.
4. **Store foundation:** create `airc-store` with ORM-backed append/replay/cursor APIs.
5. **Daemon split:** move long-lived runtime out of `airc-cli` into `airc-daemon`.
6. **Library surface:** create `airc-lib` so consumers embed Rust instead of shelling out.
7. **Work-coordination domain:** build the `airc-work` crate (typed events + projections + leases) and adapters around it. See the "Work Coordination Design" section below for the 6-PR sequence. **Do NOT port the Python queue commands verbatim** — the right shape is typed events, not CLI parsers.
8. **Transport resolver:** add health-scored resolver and prepare Tailscale discovery.

Skipping ahead to more docs, more shell, more Python, or more one-off PRs is regression.

### Proof Standard

For any claimed fix, acceptable proof is:

- local command transcript;
- Rust test or integration test;
- cross-platform evidence when the claim mentions cross-platform;
- production-record replay evidence when the claim concerns personas/cognition/coordination;
- branch/PR/issue closure evidence when the claim concerns work management.

Statements without proof do not close gaps.

## Executive Finding

The Rust substrate has a real foundation now: typed IDs, headers, envelopes, signatures, local-fs transport, TLS LAN transport, signed transport, and a CLI/daemon path with tests. That is meaningful progress.

It is not yet the flexible substrate the project needs. The current implementation still splits the system into a clean Rust protocol layer plus a large legacy operational layer in shell/Python/GitHub. That split is exactly where coordination, monitor recovery, queue/kanban, workspaces, and cross-machine behavior keep failing.

The target must be explicit:

- Rust owns runtime behavior.
- Shell is bootstrap only.
- Python is temporary compatibility only.
- GitHub is a coordination record and external adapter, not the live message bus.
- Consumers speak typed AIRC/forge-alloy contracts over AIRC, not project-specific hacks.

## Long-Term Requirement The Current System Does Not Meet

AIRC is not just a replacement chat script. The long-term requirement is a generic Rust substrate for autonomous agents, humans, apps, machines, and grids to communicate, coordinate work, exchange capabilities, replay state, and evolve without depending on Node, Python, shell, GitHub polling, or one machine's local assumptions.

The current system does not yet meet that requirement.

The target system must support:

- many agents on one machine talking at low latency;
- many machines on one LAN talking without GitHub;
- many machines across Tailscale talking without GitHub;
- future cross-grid federation;
- Continuum personas and live activities;
- OpenClaw chat/user integration;
- Hermes agent/tool integration;
- opencode/Codex/Claude style coding agents;
- humans, AIs, service daemons, game clients, render boxes, model boxes, and storage boxes as first-class peers;
- work coordination through queue/lane/kanban without stale branches and orphaned PRs;
- signed, replayable, inspectable production events;
- resource-aware coordination across CPU, RAM, disk, GPU, VRAM, network, model slots, render slots, and storage pressure;
- no accidental fallback to slow, insecure, or fake paths.

Everything below is a gap against that end state.

## Grievances

### 1. Python And Shell Still Own Too Much Runtime

The repo still depends on shell/Python for install, monitor, queue, lanes, hygiene, Codex hook plumbing, channel discovery, and status health. Some of that is tested, but tested legacy is still legacy.

This violates the Rust direction. It also creates repeated failure modes:

- state is spread across shell variables, JSON files, Python modules, and Rust structs;
- behavior differs by wrapper path;
- Windows, WSL, Git Bash, macOS, and Linux all get subtly different code paths;
- agents fix one path and leave another stale;
- new work keeps landing in the layer we are trying to delete.

Hard rule: no new runtime feature should land in shell/Python unless it directly deletes or bridges old shell/Python behavior into Rust.

Long-term unmet need:

- install/update may use shell;
- runtime messaging must not;
- runtime monitoring must not;
- runtime queue/lane must not;
- runtime hygiene must not;
- runtime identity/trust must not;
- runtime transport resolution must not;
- runtime hooks must not.

If a feature is important enough to keep, it belongs in Rust.

### 2. Work Coordination Is Not Yet A Rust Substrate

Queue, kanban, lane, manager-hat, stale PR handling, and workspace creation are still mostly bash/Python around GitHub issues and local worktrees.

That can coordinate today, but it is not yet a reusable substrate for agents across:

- same machine;
- multiple local workspaces;
- Tailscale machines;
- Windows/Mac/Linux GPU boxes;
- other grids;
- Continuum/OpenClaw/Hermes/opencode consumers.

The correct shape is a Rust coordination API with GitHub as one adapter. Issues/PRs remain durable external artifacts, but AIRC should own the local event/projection/subscription state and reconcile with GitHub instead of making every command rediscover GitHub state.

Long-term unmet need:

- a lane is a typed substrate object, not a shell convention;
- a queue card is a typed substrate object, not only a GitHub issue body;
- manager role is a typed lease/hat, not a human memory ritual;
- PR attachment is reconciled by a Rust sweep engine;
- stale PRs are surfaced and settled automatically;
- claims are leases with TTL, not comments that agents forget;
- every work item has branch, owner, evidence, blockers, next action, review state, and merge state;
- every change emits an auditable event.

### 3. Live Coordination Still Depends On Transitional Transport

Local-fs and LAN-TCP exist, but there is no complete Rust transport resolver yet. There is no production Rust mDNS/Tailscale discovery, no transport health resolver, no cross-grid relay contract, and no Rust bridge daemon that can keep persistent subscriptions alive for all consumers.

The current state proves two peers can talk. It does not yet prove the system can keep a fleet of agents online, present, routable, and work-capable across machines.

Needed shape:

- transport registry;
- health-scored resolver;
- local-fs for same-host;
- TLS LAN TCP for same LAN;
- Tailscale transport/discovery;
- cross-grid relay/transport adapter;
- GitHub only as fallback/control-plane/migration path;
- no silent downgrade to insecure or slow paths.

Long-term unmet need:

- live peer roster;
- durable membership roster;
- responder-ready roster;
- per-peer transport candidates;
- per-peer transport health;
- automatic route upgrade and downgrade by policy;
- Tailscale identity/address discovery;
- relay identity/address discovery;
- per-channel subscriptions;
- per-header subscriptions;
- per-frame priority and deadline handling;
- no GitHub routine traffic for same-machine, same-LAN, or Tailscale paths.

### 4. Windows Was Treated As Install-Only, Not Runtime-Proven

The clean install checks passing on Windows are not enough. Windows named-pipe IPC must be proven with Rust runtime tests in the same way Unix sockets are.

Status as of `rust-rewrite` `c4d4f34`: the earlier pipe-name collision finding is fixed. `crates/airc-daemon/src/ipc/transport.rs` now derives Windows pipe names from a full-path UUIDv5 hash, includes multi-home uniqueness tests, separator/case normalization tests, and a Windows-only concurrent two-home round-trip test. `.github/workflows/rust.yml` now runs `cargo test --workspace --all-features` on Ubuntu, macOS, and Windows.

Windows must be a first-class runtime target, especially for CUDA/RTX machines. Build/install success is not sufficient.

Long-term unmet need:

- Windows native daemon test;
- Windows named-pipe IPC round trip;
- Windows ACL tests for identity and peer trust files;
- Windows worktree/lane creation test;
- Windows GPU capability reporting;
- Windows Tailscale discovery;
- Windows LAN transport smoke;
- Windows install does not fork a separate product reality from macOS/Linux/WSL.

### 5. CLI/Daemon Is Accumulating Policy

The core crates are reasonably modular. The CLI/daemon layer is becoming a policy bucket: identity state, peer store, rooms, IPC, inbox, daemon state, local-fs wiring, and user command semantics all meet there.

That is acceptable for a first CLI, but not as the integration surface for Continuum/OpenClaw/Hermes. Consumers need a stable Rust library/API, not shelling out to CLI commands and parsing text.

Needed split:

- `airc-core` crate: IDs, identity, headers, basic types;
- `airc-protocol`: envelopes, frames, signatures, subscriptions;
- `airc-transport`: adapters and resolver;
- `airc-store`: durable event/projection store;
- `airc-daemon`: long-running runtime and subscription hub;
- `airc-lib`: consumer-facing Rust API;
- `airc-cli`: thin command wrapper over `airc-lib`;
- integration hooks as thin adapters, not behavior owners.

Long-term unmet need:

- CLI should be a client;
- daemon should be the runtime;
- library should be the integration API;
- store should be the source of truth;
- transports should be pluggable;
- consumers should not parse CLI output;
- consumers should not shell out for core runtime behavior.

### 6. Room/Presence Semantics Are Still Incomplete

The design correctly distinguishes membership, live presence, and responder readiness. The implementation does not yet fully deliver that.

Consumers must not render a room as if every configured persona or peer is live. A room header should be able to ask separately:

- who is a durable member;
- who is currently connected;
- who is ready to respond;
- who has relevant capability for this activity.

This matters for Continuum persona rooms, coding rooms, OpenClaw chats, Hermes agents, and VR/live/game activities. AIRC must expose the data cleanly; consumers choose the rendering.

Long-term unmet need:

- channel is generic, not only chat;
- a room can be a chat, code review, work lane, game lobby, live call, VR room, render job, model-inference pool, or grid activity;
- subscriptions are per-activity and per-capability;
- presence is transport-derived and fresh;
- readiness is consumer-announced and capability-aware;
- seeded personas are not shown as live unless they are live;
- inactive members do not fake a populated room.

### 7. Inbox And Replay Need Stronger Cursor Semantics

The daemon inbox path uses simplified lamport-only cursors and wire-level buffering. That is not enough for replay correctness under concurrency.

Required:

- cursor = `(lamport, event_id)` or stronger event-store cursor;
- channel-aware inbox filtering;
- no cross-room leakage when wires are shared;
- replay from production records, not only dummy benches;
- record/replay suitable for debugging persona cognition and consumer protocols.

Long-term unmet need:

- every production event can be captured;
- every production event can be replayed without the full live system;
- every persona cognition input can be inspected;
- every RAG/working-memory assembly can be inspected;
- every output decision can be traced to input event, memory selection, model/capability route, and resource lease;
- replay can run faster/slower/deterministically for debugging.

### 8. Trust Store Semantics Are Too Loose

Peer registry files are trust anchors. Silent key replacement for the same peer ID is too permissive. Key rotation needs an explicit operation, audit event, and confirmation path.

Also, peer store writes must be atomic and concurrency-safe. Multi-agent local processes cannot rely on load/edit/write without locking or transactional storage.

Long-term unmet need:

- trust changes are signed events;
- key rotation is explicit;
- trust roots are auditable;
- trust files are locked/transactional;
- unknown signer always fails hard under production policy;
- no dev unsigned path reachable in production.

### 9. LAN-TCP Still Has Delivery Gaps

TLS and Ed25519 are good. The LAN adapter still lacks production coordination behavior:

- no discovery;
- no reconnect/retry;
- no targeted unicast;
- no per-frame ack/replay;
- write loop can drop oversized/serialization-failed frames after `send()` accepted them into the channel.

For a grid, `send()` must mean something measurable. If the frame is too large, fail before enqueue or lift to blob storage before transport. Silent drop after acceptance is not acceptable.

Long-term unmet need:

- unicast;
- broadcast;
- ack;
- replay cursor;
- reconnect;
- route health;
- backpressure;
- priority;
- deadlines;
- blob lift before transport;
- measurable send outcome.

### 10. Docs And Implementation Are Diverging

The architecture docs describe airc-store, airc-lib, transport resolver, subscriptions, presence, and SeaORM-backed storage. The current implementation has only part of that. Some README sections still describe Python/GitHub as the product path while the design says Rust replaces it.

This is dangerous because agents keep implementing against whichever document or code path they happen to read.

The docs need a single migration truth:

- current working path;
- target Rust path;
- deprecated compatibility path;
- deletion gates;
- what must not receive new features.

Long-term unmet need:

- one source of truth for target architecture;
- one source of truth for current migration state;
- one source of truth for deprecated paths;
- every PR names which gap it closes;
- every PR updates the gap register when it changes scope;
- no orphaned design docs that do not match implementation.

### 11. Resource Management Is Not Centralized

The long-term system must coordinate CPU, RAM, disk, GPU, VRAM, network, model slots, render slots, audio/video devices, and storage pressure across the whole local/grid runtime. The current AIRC Rust stack does not yet provide that central contract.

This cannot be implemented separately inside cognition, rendering, inference, WebRTC, queues, and hygiene. That creates multiple competing schedulers and repeated resource blind spots.

Long-term unmet need:

- central resource broker/allocator;
- leases for expensive resources;
- host capability advertisement;
- host pressure telemetry;
- per-task resource requests;
- denial/defer/degrade decisions by policy;
- GPU/VRAM awareness;
- unified memory awareness on Apple Silicon;
- CUDA/Vulkan/Metal capability reporting;
- Docker/build/cache/disk policy;
- hooks for project-specific resources without hard-coding projects into AIRC.

### 12. Forge-Alloy Contract Layer Is Not Implemented

Headers and opaque bodies are the right substrate shape, but the shared contract layer is still mostly conceptual.

Long-term unmet need:

- contract registry;
- schema versioning;
- capability requirements;
- validation;
- replay semantics;
- compatibility rules;
- typed examples for `forge.work.offer`, `forge.persona.turn`, `forge.render.request`, `forge.model.infer`, `forge.webrtc.signal`;
- consumers can ignore unknown contracts without breaking delivery;
- routers inspect headers without parsing encrypted/large bodies.

### 13. Consumer Integration Is Not Proven

The docs name Continuum, OpenClaw, Hermes, opencode, Codex, and Claude, but the Rust path does not yet prove those consumers can embed cleanly.

Long-term unmet need:

- Continuum adapter proof;
- OpenClaw adapter proof;
- Hermes adapter proof;
- opencode/Codex/Claude integration proof;
- generic consumer example;
- no consumer must depend on Python internals;
- no consumer must depend on GitHub as message bus;
- no consumer must parse CLI prose;
- consumers bind to Rust API, IPC, or stable protocol envelopes.

### 14. Persona Cognition Requirements Are Not Met

AIRC is not the cognition engine, but it must carry the events that make cognition inspectable, replayable, and debuggable. The current stack does not yet prove that.

Long-term unmet need:

- every persona receives events through its own inbox;
- every persona builds its own context/memory view;
- one room event can fan out to many independent personas;
- no central "turn allocator" that decides only one persona is alive;
- RAG assembly is per persona;
- memory selection is per persona;
- response decision is per persona;
- cognition can be recorded from production;
- cognition can be replayed offline;
- no hard-coded helper persona behavior;
- no repeated canned "OK" proof masquerading as live cognition.

### 15. Multimedia And Live Activity Requirements Are Not Met

AIRC must carry signaling and coordination for live/video/game/VR activities, even though it should not carry raw media streams in message bodies.

Long-term unmet need:

- WebRTC signaling events;
- LiveKit/control events;
- avatar/live presence events;
- activity lifecycle events;
- media refs for artifacts;
- binary payload support;
- blob storage for large payloads;
- no rasterization/CPU fallback decisions hidden in AIRC;
- consumers can coordinate render/model/audio/video resources through the same substrate.

### 16. Git/Workspace Flow Is Not Enforced End-To-End

The project needs PR hygiene: claim, branch, worktree, tests, review, merge to canary, close issue, then new branch. The current tooling documents this, but does not yet enforce or automate enough of it.

Long-term unmet need:

- branch per coherent work slice;
- worktree per active claim;
- PR attached to queue/lane;
- test evidence attached to card;
- stale PR detection;
- merge readiness detection;
- canary merge tracking;
- post-merge cleanup;
- no long-lived forgotten feature branches;
- no untracked stash/temp-dir work as normal process;
- manager/agent status is visible through AIRC.

### 17. Branch Sprawl Is Destroying Continuity

The current branch layout is a symptom of the coordination failure. Work is scattered across many feature branches, some of which are already effectively historical fragments. That makes agents re-discover context, lose patches, reopen old decisions, and treat "open PR" as if it were progress.

Long-term unmet need:

- one canonical Rust rewrite integration branch;
- no feature branch older than the review window unless explicitly marked blocked;
- every feature branch maps to a queue/lane card;
- every feature branch has an owner and next action;
- every feature branch is either merged, closed, or actively blocked with evidence;
- no agent starts new Rust substrate work from a stale feature branch;
- no branch is used as long-term memory;
- branch cleanup is part of the normal workflow, not a crisis chore.

Required operating rule:

1. Sync canonical Rust branch.
2. Create short-lived branch for a coherent issue set.
3. Implement.
4. Test.
5. Review.
6. Merge back to canonical Rust branch.
7. Delete/close the branch.
8. Promote canonical branch to canary only when the slice is coherent.

## Gap List

### Rust Runtime Gaps

- `airc-store` crate with ORM entities, migrations, append/page/resume APIs. Done in slice 4; keep hardening.
- `airc-daemon` crate split out from CLI. Done in slice 5a; keep shrinking CLI policy.
- `airc-lib` high-level consumer API. Started in slice 6; not yet live-subscription complete.
- Transport resolver with health scoring.
- Tailscale adapter/discovery.
- mDNS/LAN discovery.
- Cross-grid relay/transport adapter.
- Persistent subscription hub.
- Presence and responder-ready projection.
- Record/replay API over production events.
- JSONL import/export for migration only.
- runtime config loading and validation.
- explicit production/dev policy modes.
- no unsigned production mode.
- no silent fallback policy.

### Work Coordination Gaps

- Rust queue card model and parser.
- Rust lane/kanban model with typed state machine.
- Claim leases with TTL and heartbeat.
- Manager-hat election and scheduled sweep.
- PR/lane reconciliation in Rust.
- Workspace/worktree creation in Rust or a Rust-owned command API.
- Atomic workspace registry.
- Per-project `.airc-policy` config loaded through Rust.
- Hygiene reports and cleanup policies in Rust.
- GitHub issue/PR adapter as an adapter, not command logic.
- canary merge workflow automation.
- stale branch cleanup.
- PR dependency graph.
- review load throttle.
- per-agent active work budget.
- workspace registry with locks.

### Cross-Machine And Grid Gaps

- Same-host agent roster through Rust local transport.
- Tailscale peer discovery and route selection.
- LAN-TCP reconnect.
- Remote GPU/CPU/memory capability advertisement.
- Transport health samples in the store.
- Capability-aware work offers.
- Cross-grid identity and channel addressing.
- No hard-coded machine paths.
- No GitHub polling for routine same-machine or same-network traffic.
- grid identity model.
- remote command/work negotiation.
- capability exchange.
- resource lease exchange.
- encrypted relay path.
- federation replay and dedupe.

### Consumer Integration Gaps

- Continuum: persona/chat/activity events over AIRC envelopes with forge-alloy contracts.
- Continuum: production record/replay of RAG/cognition events.
- OpenClaw: adapter mapping existing chat/thread identity into AIRC PeerId/ClientId/channel.
- Hermes: agent command/events over headers and typed payload contracts.
- opencode/Codex/Claude: inbound notifications through Rust subscription, not prompt-time polling hacks.
- Generic apps: stable `airc-lib` API, not shell command parsing.
- forge-alloy contract examples for each consumer.
- integration tests that run without GitHub when local/LAN transport is enough.
- migration map from legacy AIRC Python logs to Rust envelopes.

### Windows Gaps

- Runtime named-pipe IPC test on Windows. Done for daemon IPC concurrent two-home round trip; keep adding queue/lane/workspace coverage.
- Pipe names derived from full scoped path or explicit stable ID, not parent basename. Done for daemon IPC.
- Windows ACL hardening for key/trust files.
- Windows queue/lane/workspace smoke test.
- CUDA/RTX machine readiness script that proves build, daemon, transport, and GPU capability advertisement.

### Security Gaps

- group encryption for room broadcasts;
- forward secrecy beyond current signing/TLS baseline;
- hardware-backed identity where available;
- at-rest encryption for store/blobs;
- explicit key rotation protocol;
- signed trust changes;
- capability-scoped permissions;
- consumer contract validation;
- audit log for admin/trust/work changes.

### Performance/VDD Gaps

- append latency benchmarks;
- subscription latency benchmarks;
- 4/8/16 concurrent writer tests;
- 10/20 local peer tests;
- Tailscale cross-machine latency test;
- large replay test;
- resource pressure test;
- idle CPU test;
- disk growth test;
- memory/RSS test;
- no performance claim without reproducible command and artifact.

## Audit Refresh: 2026-05-19

Current branch audited: `rust-rewrite` at `c4d4f34`.

Local verification run:

```bash
cargo test --workspace --all-features
```

Result: 171 Rust tests plus doctests passed locally. This covers `airc-blobs`, `airc-core`, `airc-protocol`, `airc-transport`, `airc-store`, `airc-daemon`, `airc-lib`, and `airc-cli` e2e.

Verified strengths:

- The Rust substrate now has real crates for protocol, transport, store, daemon, CLI, and consumer facade.
- Envelopes have the right generic shape for future consumers: UUID IDs, `PeerId`/`ClientId`, channel, target, headers, opaque body, media refs, and signatures.
- Headers are a `BTreeMap<String, String>` with namespace conventions and header filters, which is the right primitive for OpenClaw/Hermes/Continuum/opencode routing without body parsing.
- Strict signing is the production default in CLI/lib paths; LAN-TCP uses TLS with pinned peer identities.
- `airc-store` gives durable append/page/resume over `(lamport, event_id)` with room filtering.
- The daemon now persists inbox events through `airc-store` instead of a ring buffer.
- Windows daemon IPC now has a real runtime test path in CI.

Critical remaining gaps:

- The public `airc` product path now routes through Rust for identity, config, logs, monitor formatting, Codex hooks, queue/work helpers, and install-time setup. The legacy GitHub-as-wire command has been deleted; remaining shell should keep shrinking toward install/bootstrap only.
- `airc-lib` now owns in-process send/subscribe/replay, route selection, local-fs execution, LAN/Tailscale-class TCP execution, route health, invite endpoint metadata, and a daemon-attached SDK path for send/page/resume through typed daemon IPC. Persistent live subscription streams over daemon IPC are still not complete.
- `airc-cli` is thinner for local, LAN, and daemon-backed msg/inbox paths. Remaining user-facing commands must continue moving onto SDK surfaces instead of constructing substrate state directly.
- Peer trust rotation is still too permissive: `peers_store::add` silently replaces a pubkey for the same `PeerId`. That must become an explicit signed rotation/audit operation.
- Route resolver basics exist, and LAN listen/connect feed health. Same-host and bound-LAN discovery now populate route health/endpoints without GitHub or Tailscale. Missing: optional Tailscale, relay, UDP/WebRTC, and Reticulum probes without manual flags.
- Gist is modeled as invite/rendezvous only, and the Rust transport crate now exposes an invite-file store rather than a runtime frame transport. Remaining work: wire user-facing invite commands fully through the SDK path and add signed/audited invite validation.
- Relay for different tailnets/NAT is not implemented. This is required before cross-grid and unreliable-network claims are credible.
- UDP and WebRTC datachannel adapters are modeled but not implemented. They are required before Continuum live-mode, game, and realtime control integration can be considered ready.
- Presence/live roster/responder-ready state is not implemented as a Rust projection.
- Queue/lane/workspace/kanban are still legacy operational surfaces, not Rust substrate objects.
- No Continuum/OpenClaw/Hermes/opencode Rust embedding examples prove future integrations.
- No forge-alloy contract registry or validation layer exists yet; headers/body shape is ready, semantics are still consumer convention.
- No resource broker/capability lease layer exists for CPU/GPU/memory/disk across inference, render, audio, and work coordination.
- No production persona record/replay path exists. AIRC can store transcript events, but Continuum cognition/RAG replay is not implemented on top.

## Acceptance Gates

### Gate 1: Rust Hot Path

Pass when two local agents can:

1. start Rust daemon;
2. discover each other;
3. exchange signed messages;
4. see presence;
5. list live peers;
6. replay transcript;
7. do this with no Python in the runtime path.

### Gate 2: Work Coordination

Pass when an agent can:

1. view queue/lane state through Rust;
2. claim a lane;
3. create an isolated workspace;
4. heartbeat ownership;
5. open/attach a PR;
6. have sweep reconcile PR status;
7. release or land the work;
8. leave an auditable event trail.

GitHub may store the issue/PR record, but Rust must own local state, validation, and replay.

### Gate 3: Cross-Machine

Pass when agents on two machines over Tailscale can:

1. discover each other;
2. exchange signed frames without GitHub as the message bus;
3. report transport health;
4. route messages by capability;
5. survive one peer restart and reconnect;
6. preserve replay.

### Gate 4: Consumer Embedding

Pass when a small consumer app can link `airc-lib` and:

1. create/load identity;
2. join a channel;
3. send typed body with headers;
4. subscribe by header/channel/kind;
5. fetch replay;
6. use blobs;
7. never shell out.

### Gate 5: Python/Shell Deletion

Pass when:

- shell only bootstraps install/update/uninstall;
- Python has no runtime ownership of messaging, monitor, queue, lane, hygiene, identity, transport, or hooks;
- remaining Python is either deleted or explicitly marked migration-only with an issue and removal gate;
- new PRs adding runtime behavior to Python/shell fail review.

## Immediate Remediation Plan

1. Freeze shell/Python runtime feature work.
2. Create `airc-store` and move event append/replay/cursor there first.
3. Split daemon into `airc-daemon` and keep CLI thin.
4. Move queue/lane models into Rust as typed data and parsers.
5. Add workspace sanitation/drain as a first-class Rust feature: leased workspaces report disk usage, policy names safe cleanup targets, and low-space pressure can trigger dry-run reports or approved drains without waiting for a human.
6. Add Windows runtime IPC tests before claiming Windows support.
7. Add transport resolver and Tailscale discovery before broader grid claims.
8. Add consumer smoke examples for Continuum/OpenClaw/Hermes/opencode.
9. Add migration/deletion board tracking every remaining Python/shell module.

## Work Coordination Design (2026-05-19)

Slice 7 as originally listed ("port card/lane state models and parsers into Rust") is **wrong as scoped**. Porting the Python queue commands verbatim recreates the mess in Rust. The right shape is a typed work-coordination *domain* sitting on the substrate, with GitHub demoted from source-of-truth to one adapter among many. Codex authored this design; it is binding.

### Three layers

1. **AIRC substrate** — identity, channels, signed envelopes, headers, subscriptions, event store, transport resolver. Already mostly built (slices 1–6c).
2. **AIRC work-coordination domain** — queue cards, lanes, claims, leases, workspaces, PR links, stale detection, manager-hat status, hygiene/resource reports. **New crate: `airc-work`.**
3. **Adapters** — GitHub issues/PRs, local git worktrees, Codex hooks, Claude monitor, Continuum activity/event subsystem, future OpenClaw/Hermes/opencode bridges.

**The key rule:** GitHub is an adapter, not the source of truth for runtime coordination. AIRC owns local typed state and event replay. GitHub mirrors durable/public artifacts.

### Core event model

Work events ride on AIRC envelopes with headers like:

```
forge.body_hint = forge.work.claim
airc.domain     = work
airc.channel    = <project/channel>
airc.priority   = p1
airc.trace_id   = <uuid>
work.repo       = CambrianTech/continuum
work.lane       = rust-cognition
```

Bodies are typed alloy contracts:

- `WorkCardCreated`
- `WorkCardClaimed`
- `WorkCardReleased`
- `WorkCardHeartbeat`
- `LaneCreated`
- `LaneStateChanged`
- `WorkspaceAllocated`
- `WorkspaceReleased`
- `WorkspacePressureReported`
- `WorkspaceDrainRequested`
- `WorkspaceDrainCompleted`
- `PullRequestLinked`
- `PullRequestMerged`
- `HygieneReportRecorded`
- `ManagerHatClaimed`
- `ManagerHatReleased`

Projections (read models) are built by replaying events from `airc-store`:

- `WorkBoardProjection`
- `LaneProjection`
- `WorkspaceRegistryProjection`
- `WorkspacePressureProjection`
- `PeerWorkloadProjection`
- `StaleWorkProjection`
- `GitHubMirrorProjection`

No command should "calculate the world" from GitHub every time. Commands query projections fed by event replay.

### Workspace design

A workspace is a leased resource, not a folder convention.

```rust
WorkspaceLease {
    lease_id: LeaseId,
    owner: PeerId,
    repo: RepoId,
    branch: BranchName,
    base: BranchName,
    path: PathBuf,
    status: Allocated | Active | Released | Orphaned | Failed,
    created_at_ms: u64,
    heartbeat_at_ms: u64,
    disk_bytes: Option<u64>,
}
```

Events: `workspace.requested`, `workspace.allocated`, `workspace.heartbeat`, `workspace.released`, `workspace.orphan_detected`, `workspace.cleaned`.

Disk hygiene, stale-branch cleanup, and "who is doing what" all derive from the same event stream.

Workspace sanitation is not optional polish. A machine may host 20+ local agents, each with lanes, worktrees, Rust targets, Docker layers, model caches, browser traces, and project sandboxes. AIRC must know what it allocated and provide a drain path for anything rebuildable. The feature shape:

- AIRC-created git worktrees live under the single home root `~/.airc/worktrees/<repo>/<lease-or-branch>` by default. Repo-local `.airc/` remains project scope state; ad hoc sibling roots such as `~/Development/.../airc-worktrees` are not the convention.
- every workspace allocation records its root path, repo, branch, owner peer, lease id, created timestamp, heartbeat timestamp, and expected PR/issue links;
- worktree drains are policy-backed: merged/closed PRs, released leases, expired heartbeats, and clean git status may be deleted automatically; dirty or unknown worktrees are report-only until an explicit policy admits them;
- every workspace lease records disk usage and cache roots it owns;
- every drain candidate is typed as `RebuildableCache`, `GeneratedArtifact`, `DownloadedDependency`, `DockerLayer`, `ModelCache`, `TraceArtifact`, or `Unknown`;
- default policy may drain only safe rebuildable caches;
- destructive or ambiguous drains require explicit policy opt-in;
- `airc workspace report` and `airc hygiene report` expose the same projection;
- low-space pressure emits `WorkspacePressureReported`, then policy may emit `WorkspaceDrainRequested`;
- completed cleanup records bytes reclaimed, paths touched, and policy rule id;
- dry-run and record/replay are required so cleanup decisions can be inspected after the fact.

This is the "everything needs a drain" rule for the local grid. It applies to workspaces first, then model caches, renderer caches, Docker/build caches, and consumer-specific sandboxes through hooks.

Local reference while implementing: Joel's `~/Development/ddd.py` ("Developer Disk Declutter") is not product code, but it is a useful inventory of real cleanup pressure on the Mac: Xcode DerivedData and DeviceSupport, iOS simulators, Android SDK/NDK images, Docker prune, Cargo/Gradle/npm/pip caches, browser/tool logs, Playwright browsers, and large project `target` directories. The Rust feature should generalize those categories into policy-backed drain targets rather than hard-code that machine's paths.

### Kanban as projection

Kanban is a *projection*, not a primary object. Cards are typed work items; lanes are grouping/priority/state machines; board views are derived.

```
CardState = Open | Claimed | InProgress | Blocked | Review | Merged | Closed
LaneState = Planned | Active | Blocked | Landing | Done
```

**Claims are leases with TTL.** A peer does not "own" forever because it wrote a GitHub comment. It owns while it heartbeats. Closes grievance §2's "claims are leases with TTL, not comments that agents forget."

### Subscriptions serve everything

Monitors, hooks, and Continuum all subscribe to the same store with header/channel filters:

- **Codex hook**: `airc.domain in [chat, work]`, `target` includes self, since cursor.
- **Claude monitor**: `channel = current project + general`, `kinds = Message/Event/Control`.
- **Continuum persona**: `continuum.activity = <activity_id>`, `forge.body_hint` prefix `forge.persona.`.
- **Workspace manager**: `airc.domain = work`, `forge.body_hint` prefix `forge.work.`.

No bespoke hook polling logic. No separate monitor channel model. No Continuum event subsystem fighting AIRC. One event store, one cursor contract, many subscription filters.

### Slice 7 PR sequence

Replaces the single-slice "Queue/lane Rust model" line in the First Remediation Slices list.

1. **PR 1 — `airc-work` crate foundation.** Typed work events, workspace events, typed lane/card state, projection traits, in-memory projection tests. No GitHub yet. **Owner: Codex.**
2. **PR 2 — store-backed projections.** Replay from `airc-store`, cursor-based update, board query APIs, stale lease detection.
3. **PR 3 — workspace sanitation/drain.** Rust-owned workspace pressure projection, policy-loaded drain candidates, dry-run reports, typed drain events, and tests proving safe caches can be reclaimed without touching source or untracked work.
   - **Sub-PR 3a — drain typing (this PR).** Typed events: `WorkspacePressureReported`, `WorkspaceDrainRequested`, `WorkspaceDrainCompleted`. Typed model: `DrainCandidateCategory` (closed enum incl. `Unknown`), `DrainCandidate`, `PressureLevel`, `DrainOutcome`. Projection extends `WorkBoardProjection` with `workspace_pressure`, `pending_drains`, `drain_history` — all keyed by `WorkspaceId` independent of the card+claim lease flow so peers without leases can participate in hygiene. Codec roundtrip + projection sequence tests. No runtime detector or executor; that's the next sub-PR.
   - **Sub-PR 3b — workspace lease decoupling (follow-up).** Today's `WorkspaceLease` requires `card_id` + `claim_id`. Peers that allocate workspaces outside the card flow can emit pressure/drain events but cannot register the workspace itself. Needed: a lightweight workspace registration event without card/claim, plus a `WorkspaceStatus::External` (or equivalent) variant. Out of scope for 3a — it's a wider model change with downstream impact on `WorkspaceRequested`.
   - **Sub-PR 3c — pressure detector + drain executor.** Runtime that probes disk, emits `WorkspacePressureReported`, evaluates policy (default = `RebuildableCache` + `DownloadedDependency`), and executes `WorkspaceDrainRequested` → `WorkspaceDrainCompleted`. Dry-run path lands first.
4. **PR 4 — CLI thin wrapper.** `airc work list`, `airc work claim`, `airc lane status`, `airc workspace list`, `airc workspace report`, `airc hygiene report`, `airc hygiene clean --dry-run` — all backed by `airc-work`, no Python touched.
5. **PR 5 — GitHub adapter.** Mirror card state to issues/PRs; reconcile PR status into events; GitHub is adapter-only.
   - **Sub-PR 5a — git/PR event contracts.** Typed events for
     `GitCommitObserved`, `GitBranchMoved`, `GitDirtyStateChanged`,
     `PullRequestCheckSuiteChanged`, `PullRequestReviewSubmitted`, and
     `PullRequestMergeStateChanged`, plus routeable headers and
     projection records. No `gh` calls and no local git watcher in
     this slice.
   - **Sub-PR 5b — local git watcher adapter.** Observe branch head,
     commit, and dirty-state changes for AIRC-managed workspaces and
     emit the 5a events. Implemented as `airc-work::local_git` plus
     `airc-lib::Airc::observe_local_git_workspace`: the adapter reads a
     local git worktree, compares it to the caller's prior snapshot, and
     publishes only typed work events for actual changes. CLI/monitor
     surfaces remain consumers of this library API rather than owning
     git parsing.
   - **Sub-PR 5c — GitHub PR adapter.** Reconcile PR/check/review/merge
     state into the 5a PR events. GitHub remains an adapter, not the
     runtime source of truth.
6. **PR 6 — monitor/hook subscriptions.** Codex hook becomes `airc codex-hook` consuming an `airc-work`-aware subscription; monitor reads typed subscriptions; no Python hook path remains.
   - **Phase 2 lifecycle cursor slice.** Runtime consumers persist
     cursors through `airc-store` and emit `SubscriptionAdvanced` as a
     durable lifecycle event when they advance. Feeds/hooks that know
     the source event use `Airc::save_runtime_cursor_for_event` so a
     cursor advance caused by `SubscriptionAdvanced` is stored without
     recursively emitting another cursor event.
   - **Phase 2 room-part slice.** `airc-lib::Airc::part_channel`
     removes the subscription through the ORM-backed `SubscriptionSet`,
     preserves identity/trust/other rooms, refreshes the account
     presence snapshot, and emits durable `RoomParted`. The public
     `airc part [room]` command is a thin wrapper over that API.
7. **PR 7 — Continuum bridge.** Continuum event subsystem consumes AIRC subscriptions directly. Persona inboxes are AIRC channel/header subscriptions plus Continuum-specific projection/RAG assembly.

### What this replaces in the previous plan

- **Old Immediate Remediation Plan item 4** ("Move queue/lane models into Rust as typed data and parsers") is too narrow — replaced by the 6-PR sequence above.
- **Old Work Coordination Gaps** list items (Rust queue card model, lane state machine, claim leases, manager-hat election, PR/lane reconciliation, workspace registry) all map onto the typed events + projections above. No verbatim Python port.
- **Gh-gist transport question** is reframed: gh-gist was confused as a wire, but in the new design GitHub is an adapter, not a transport. Cross-machine wire goes to Tailscale/LAN per grievance §3; the GitHub adapter only mirrors durable artifacts (issues/PRs).

### Python-removal sequencing under this design

Slice 7 (PR 1–7) closes most of what Python currently owns at runtime:

- queue/lane/workspace/kanban — `airc-work` (PR 1–3)
- hygiene/sweep/manager-hat — projections + lease TTLs (PR 1–3)
- GitHub token state belongs only in rendezvous/artifact adapters; it must not re-enter the runtime wire path
- Codex hooks — Rust subscription (PR 6)
- Claude monitor / monitor channel — Rust subscription (PR 6)

Once PR 1–6 land, the only Python left is install/update/uninstall bootstrapping plus migration shims. That clears Gate 5.

## Non-Negotiables

- No silent fallback to insecure/plain/slow paths.
- No hard-coded machine paths.
- No consumer-specific semantics in AIRC core.
- No raw SQL in command handlers or consumers.
- No CLI text parsing as the integration API.
- No Python/shell expansion of runtime behavior.
- Every production behavior must be recordable and replayable.
- Every performance claim needs a reproducible measurement.
- Every cross-machine claim needs a cross-machine test.
