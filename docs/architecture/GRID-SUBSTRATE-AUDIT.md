# Grid Substrate Audit

Status: active steering document for `rust-rewrite`.

This audit locks the intended product boundary:

- AIRC is the grid substrate: identity, trust, routes, presence,
  typed envelopes, subscriptions, replay, and transport selection.
- Agents are one consumer class, not the substrate's center.
- Continuum, OpenClaw, Hermes, opencode, Claude, Codex, games,
  live rooms, render nodes, and model hosts all use the same substrate
  surface.
- Consumer vocabularies live above AIRC as typed contracts
  (`forge.*`, `continuum.*`, `openclaw.*`, etc.). AIRC routes by
  headers and subscription filters; it does not understand those
  domains.

The short version: AIRC must be a local-first, tailnet-capable, p2p-
ready event and command substrate that can carry Continuum's room,
persona, model, LoRA, render, game, and tool traffic without becoming
Continuum-specific.

Priority order matters:

1. **Same machine / same user account:** must be boring, fast, and
   automatic. This is the critical path for Codex, Claude, local
   Continuum, local OpenClaw, local Hermes, and per-machine grids.
2. **Tailnet / LAN remote:** the next practical route. We expect
   Tailscale and LAN transports to cover most multi-machine personal
   grids before broader public p2p is required.
3. **Grid-to-grid p2p:** important for multi-human Continuum/OpenClaw
   grids, but later. Do not block local or tailnet reliability on
   global discovery/federation.

## Non-Negotiables

1. `airc join` is the default recovery and live setup verb.
   No required `--attach`, `watch`, `monitor`, or per-runtime flag for
   the normal path.
2. Routine same-machine traffic never depends on GitHub. GitHub is
   invite/rendezvous metadata only.
3. Local route wins when available. Tailnet/LAN routes are next.
   Relay/public p2p routes exist to cross boundaries, not to replace
   the local fast path.
4. No silent fallback. A route may be selected by policy; a fallback
   must be explicit, observable, and testable.
5. Rust substrate is the source of truth. Shell/Python are install or
   migration tools only and should trend to zero runtime ownership.
6. Consumer integrations must be thin. Continuum/OpenClaw/Hermes
   should call `airc-lib`, not parse CLI text or reach into AIRC state
   files.
7. Cursor replay and live push are first-class. Anything visible in
   production should be inspectable, replayable, and debuggable
   without a full UI stack.
   Claude currently receives live push through Monitor. Codex does
   not expose an equivalent runtime interrupt primitive, so the
   installed Codex contract is a long-running `airc join` feed tool
   session plus prompt-boundary hook catch-up. Treat that as a
   runtime integration gap, not as a substrate transport failure.
8. Code organization must preserve the substrate boundary. If a file
   starts naming consumers, move that surface into an integration
   crate/module.
9. Durable substrate data uses the store/ORM boundary. JSON is allowed
   for wire payload encoding, install/config bootstrap, and external
   invite documents; it is not acceptable for runtime cursors, trust
   state, subscriptions, presence registries, or replay checkpoints.
10. CI proves the production path; it does not substitute around it.
    A test that disables a behavior the production path enables is
    blind to the bugs that behavior creates. Emulate the production
    shape (Monitor-style streaming consumer, daemon-attached send,
    cross-machine route resolution) under test; reach for
    `AIRC_NO_ATTACH` or similar disable-flags only when the test is
    explicitly proving the script/setup-only path. If we can't
    emulate it cheaply, that's a substrate gap, not a test cheat.

## Current Drift

The Rust rewrite rebuilt important primitives, but recent work drifted
into agent-shaped seams:

- `airc-cli` owns Codex-specific modules and runtime heuristics.
- Join live-feed decisions currently sit in `commands.rs`.
- Hook/monitor behavior is still easier to reason about as agent
  plumbing than as a general subscription consumer.
- Lifecycle events exist implicitly in logs and status output, not as
  typed events consumers can subscribe to.
- Command/request-response behavior is still a convention, not a small
  SDK primitive.

These are fixable, but they must be fixed as substrate cleanup, not as
more agent patches.

## Phase 1 — Integration Boundary Extraction

Goal: stop making the substrate name Codex/Claude directly.

Work:

- Add `integrations-common` crate/module for runtime detection,
  runtime client identity, store-backed cursor ownership, and hook/feed
  conventions shared by agent integrations.
- Move Codex-specific surfaces out of `airc-cli`:
  - `codex_*.rs`
  - Codex hook JSON/config mutation
  - Codex-specific feed/cursor policy
- Define the Codex feed contract explicitly: `airc join` is the live
  feed, `airc codex-hook poll --wait-ms N` is the bounded mid-turn
  tool-callable feed, the UserPromptSubmit hook is prompt-boundary
  catch-up, and true wake-on-AIRC requires Codex runtime support
  outside the substrate.
- Move generic runtime context detection out of `commands.rs`.
  `airc join` should call a small substrate-facing classifier, not own
  env/process heuristics inline.
- Keep `airc-cli` as a thin command surface over `airc-lib` plus
  integration install commands.

Acceptance gates:

- `airc-cli/src/commands.rs` no longer contains Codex/Claude-specific
  runtime marker lists.
- Generic runtime context is represented by a typed enum, not boolean
  soup:

  ```rust
  pub enum RuntimeContext {
      InteractiveTerminal,
      Agent { kind: AgentRuntimeKind, client_id: RuntimeClientId },
      Automation,
      TestHarness,
  }
  ```

- `airc join` behavior is selected from `RuntimeContext`.
- Existing Codex and Claude behavior remains green in public install
  proof tests.

## Phase 2 — Substrate Events And Subscription API

Goal: make lifecycle and room membership observable to every consumer,
not hidden in CLI prose.

Work:

- Add typed lifecycle events:
  - `PeerArrived`
  - `PeerDeparted`
  - `WireEstablished`
  - `WireLost`
  - `RoomJoined`
  - `RoomParted`
  - `SubscriptionAdvanced`
- Add subscription query API on `airc-lib`:
  - `Airc::subscriptions()`
  - `Airc::is_subscribed(ChannelName)`
  - `Airc::default_room()`
  - `Airc::subscription_cursor(ChannelName)`
- Make `Airc::open` accept or derive `RuntimeContext`, so consumers
  do not reimplement runtime identity/client-id logic.
- Add typed repository/work tracking adapters:
  - Git events: branch moved, commit observed, worktree leased/drained,
    dirty state changed.
  - PR events: check suite queued/running/passed/failed, review
    requested/submitted, merge state changed.
  - Kanban/work events: card claimed, card blocked, lane changed,
    status advanced.
  These are event producers over the substrate, not ad hoc polling in
  agent scripts. Agents subscribe by repo/lane/PR headers and receive
  state changes the same way they receive chat, monitor, and lifecycle
  events.

Status:

- `airc-lib` subscription query API landed in #911.
- Typed lifecycle events landed in #914.
- Lifecycle emit points are being wired as Phase 2 sub-slices:
  `RoomJoined`, `PeerArrived`, `PeerDeparted`, `WireEstablished`,
  `RoomParted`, and `SubscriptionAdvanced` now emit durable lifecycle events from
  substrate code. Cursor advancement uses
  `Airc::save_runtime_cursor_for_event` where the source event is known
  so advancing past a `SubscriptionAdvanced` lifecycle event stores the
  cursor without recursively emitting another cursor event.
  `RoomParted` is emitted by the ORM-backed `Airc::part_channel`
  subscription transition and surfaced through the thin `airc part`
  CLI command.
  `PeerDeparted` is emitted by the ORM-backed `Airc::remove_peer`
  trust transition and surfaced through `airc peer remove`, including
  live daemon verifier sync when a daemon is running.
- Typed git/PR event contracts are being added in `airc-work`:
  `GitCommitObserved`, `GitBranchMoved`, `GitDirtyStateChanged`,
  `PullRequestCheckSuiteChanged`, `PullRequestReviewSubmitted`, and
  `PullRequestMergeStateChanged`. This is the contract/projection
  layer only; the GitHub adapter and local git watcher are separate
  producers that must emit these events instead of polling inline.
- The local producer is `airc-work::local_git`, surfaced through
  `airc-lib::Airc::observe_local_git_workspace`. It records branch,
  head commit, commit summary, and dirty/untracked counts as typed
  events, with duplicate suppression based on an explicit prior
  snapshot. Monitor, hooks, Continuum, OpenClaw, and Hermes should call
  this API or subscribe to its events; they should not each poll git and
  invent their own state model.

Acceptance gates:

- Continuum can subscribe to room/lifecycle events through `airc-lib`
  without shelling out.
- OpenClaw can list joined rooms and presence from typed API calls.
- Agent monitor/feed code consumes the same lifecycle events.
- Agents can track CI/PR/kanban state through subscriptions instead of
  each runtime polling GitHub independently.
- `airc status` is a renderer over typed state, not an owner of state.

## Phase 3 — Reliability And Error Shape

Goal: make failures explicit and measurable.

Work:

- Split broad error types into typed sub-enums:
  - transport errors
  - trust errors
  - subscription errors
  - store/replay errors
  - runtime context errors
- Add broadcast backpressure metrics:
  - subscriber lag count
  - dropped event count where loss is allowed
  - oldest retained cursor
  - per-subscriber cursor age
- Replace silent ingest warnings with typed diagnostics that can be
  emitted as events and surfaced in `airc doctor`.
- Audit `Mutex`/lock scopes across `.await`; split or narrow where a
  lock crosses IO.
- Reduce avoidable `event.clone()` in hot broadcast paths.
- Add scheduled drains for stale worktrees, stale beacons, stale
  daemons, and stale temp files.

Acceptance gates:

- No routine substrate error is only printed to stderr.
- Consumers can distinguish route unavailable from auth failure from
  replay cursor expired.
- `airc doctor` can report backpressure and stale resource pressure.
- Workspace drain policy is exercised against real `~/.airc/worktrees`.

## Phase 3.5 — SeaORM End-to-End (Kill the JSON Files)

Goal: one durable, transactional, indexed source of truth for **all**
substrate state. Today only the events table goes through SeaORM —
every other persisted collection is a hand-rolled JSON file with
temp-write-and-rename, no index, no transaction, no schema. That's
"ORM as append log," not "ORM as the state model."

This phase is the elegance baseline: stop the per-collection drift.

Move to SeaORM entities behind the `airc-store::EventStore` contract
(one row, one table, one transaction). SQLite is the v1 embedded
backend, not the architecture; consumers talk to store traits/APIs, not
backend files:

| Today (JSON file + temp-rename) | Phase 3.5 (SeaORM table) |
|---|---|
| `peers.json` | `peer_trust` + `peer_rotation_audit` (done in #883) |
| `subscriptions.json` | `subscriptions` table (done in store-backed subscriptions cut) |
| `room.json` (current room marker) | `subscriptions.is_default` (done in store-backed subscriptions cut) |
| `identity.json` (singleton metadata) | `local_identity` table behind the store API (done in store-backed local identity cut; key material stays in `identity.key`) |
| `mesh_identity` cache file | `mesh_identity` table (done in store-backed mesh identity cut) |
| `account_registry/*.json` | `account_registry` table (done in store-backed account registry cut) |
| `coordinator/*.beacon` | `beacons` + `beacon_channels` tables (done in store-backed coordinator beacon cut) |
| `accounts/*/refresh.lock` | `refresh_locks` table (done in store-backed coordinator lock cut) |
| `codex_hook_cursor.json` + `join_feed_cursor.{client}.json` | `runtime_cursors` table (done in cursor cut) |
| `airc config ...` JSON editor | removed from the public CLI; typed commands and store APIs own runtime state |

What dies the day this lands:
- Every `tmp + rename` pattern (DB transaction is atomic).
- Every "re-read this file on every command" cache miss.
- The Windows replace-on-rename special-casing.
- Cursor file sprawl (one row per `(client_id, channel)`).
- Generic `config.json` mutation as a runtime API.
- "Which file owns peers" ambiguity (peers are in a table; foreign
  keys make ownership obvious).
- Drains: `DELETE FROM beacons WHERE ttl_expires_at < ?` replaces
  every "scan dir, decode each file, filter, unlink" loop.

What Continuum's bus gets day-1:
- A `sea_orm::DatabaseConnection` it can hold and query directly
  through `airc-lib`. No CLI parsing, no JSON file reads.
- `SELECT * FROM peers WHERE last_seen_at > ?` instead of file globbing.
- Lifecycle events become rows in the same events table with a
  typed `kind`, observable via the same subscription stream.

Acceptance gates:

- `crates/airc-lib` contains zero `std::fs::write` calls in hot paths
  (CLI install/migration helpers excepted).
- All persisted collections have a SeaORM entity with migration.
- One transaction can update peers + subscriptions + cursors
  atomically (e.g. `airc join` is one DB transaction, not 4 separate
  file rewrites).
- A consumer (Continuum's bus) can mount the same DB read-only and
  query peers/subs/rooms without CLI mediation.

## Phase 3.6 — Performance Hotpaths (Make It FAST)

The substrate has to carry Continuum's whole grid — events,
commands, presence, p2p. Joel's bar: **fast**. The audit found
five concrete hotpaths and seven kill-list patterns.

### Top 5 perf problems (worst first)

1. **Synchronous file I/O on every CLI/subscribe call** —
   peer trust moved to SeaORM in #883; subscriptions/default-room,
   local identity metadata, mesh identity, account registry,
   runtime cursors, and coordinator beacons now use store tables.
   Remaining file-backed surfaces are install/config compatibility
   helpers and remote wire payloads, not the local runtime truth.

2. **Double clone of `TranscriptEvent` on every ingest** — closed by
   the Arc live-event cut. Store append still receives the owned
   event it persists, but live fan-out now broadcasts
   `Arc<TranscriptEvent>` so subscriber delivery is pointer clone
   rather than full event clone.

3. **`RwLock<PeerKeyRegistry>` at process-global granularity** —
   closed by the concurrent peer-registry cut. `PeerKeyRegistry` now
   owns a sharded `DashMap`; signed transport, TLS verifiers, relay,
   daemon, and SDK handles hold `Arc<PeerKeyRegistry>` directly. Frame
   verification no longer serializes on a process-global read lock.

4. **`event.clone()` on broadcast fan-out** — closed with Top #2.
   `EventStream` and `FilteredEventStream` now yield
   `Arc<TranscriptEvent>`; owned clones happen only at explicit
   compatibility boundaries such as command-bus reply return values
   and hook batch vectors.

5. **Linear peer dedup at startup** — closed by the set-backed dedup
   cut. `Airc::open` uses a `HashSet` while loading peer trust rows,
   and the live broadcast duplicate guard uses `BroadcastDeduper`
   (`VecDeque` eviction + `HashSet` membership) instead of scanning
   the whole recent-event ring on every ingest.

### Substrate-wide patterns to kill (priority order)

1. **Synchronous file I/O in async functions** — local runtime state
   must stay store-backed. Peer trust, subscriptions, local identity
   metadata, mesh identity, account registry, runtime cursors, and
   coordinator beacons are now store-backed.
2. **`RwLock<HashMap>` at global granularity** — closed for
   PeerKeyRegistry and the route resolver tables. Route health,
   advertised endpoints, and imported invites now own internal
   `DashMap` storage and are held by `Arc<Table>` at the SDK boundary.
   Remaining lifecycle maps should avoid holding locks across async
   I/O; wire/LAN subscriber setup now checks under lock, performs
   transport setup outside the critical section, then re-checks before
   insertion.
3. **Full JSON file rewrites on every mutate** — removed from peer
   trust, subscriptions/default-room, account registry, and
   coordinator beacons. Keep pushing remaining config/install helpers
   toward typed store-backed or wire-payload-only boundaries.
4. **`event.clone()` on broadcast hot path** — `Arc<TranscriptEvent>`
   internally; broadcast is `Arc::clone`.
5. **Linear scans for dedup** — closed for peer startup dedup and
   subscription room/wire collection. `Airc::open` uses `HashSet` for
   peer enrollment, and subscription helpers now keep deterministic
   output order while using set-backed membership instead of
   `Vec::contains` scans. Live/persisted `EventFilter` channel
   membership is also set-backed, so multi-room monitor/hook/consumer
   streams do not pay a linear channel scan on every event.
6. **Verification lock held across async I/O** — closed for
   `PeerKeyRegistry`. `signed.rs` verifies against the registry's
   internal concurrent map directly; there is no external lock to hold
   while draining the stream.
7. **Untracked `tokio::spawn` tasks** — closed for local ingest.
   `spawn_frame_ingest` now returns an owned `IngestTask`; dropping
   the SDK subscriber aborts the task instead of detaching it from the
   handle lifecycle, and explicit wire teardown still sends a shutdown
   signal before waiting briefly for `WireLost` emission. Future
   transport/server loops should use the same owned-task pattern (or a
   scoped `JoinSet`) rather than discarding `JoinHandle`s.

### Layering rot (architectural)

**Status after Phase 3.5**: peer-trust + identity ORM cuts
(#883/#885/#902) softened the `airc-lib`/`airc-daemon` interface
— the `peers_store` is now a thin shim over `airc-store`
methods, not a parallel JSON store. The deeper issue (lib
naming daemon types) is still present and worth a separate
follow-up.

- **`airc-lib` imports daemon runtime internals** — CLOSED
  in the IPC crate cut. `DaemonClient`, request/response enums, the
  length-framed codec, and cross-platform IPC transport now live in
  `airc-ipc`, so daemon-attached SDK mode no longer depends on the
  daemon runtime crate for IPC. The identity crate cut moved
  `LocalIdentity` / `IdentityError` into `airc-identity`, so consumers
  can open local identities without importing daemon runtime state.
  The peer-trust crate cut moved peer enrollment, removal, and signed
  rotation into `airc-trust`, so `airc-lib` no longer names daemon
  runtime internals for trust storage either.
- **`airc-transport::signed` holds peer trust through
  `Arc<PeerKeyRegistry>`** — CLOSED for global-lock contention. Key
  rotation mutates the shared registry directly, so transport verifiers
  observe updates without swapping an outer lock guard. A future
  delegate can still narrow ownership further, but the hot-path
  serialization point is gone.
- **Daemon IPC is line-delimited JSON without length-framing** —
  CLOSED in the IPC framing cut. `airc-ipc::codec` now owns a single
  length-prefixed CBOR frame format for request/response RPC and
  long-lived attach streams, with a bounded max frame size.

### SeaORM perf notes (for Phase 3.5)

If moving peers/subs/rooms/identity/cursors/beacons into SeaORM:

- **Indexes**: `(peer_id)` unique on peers; `(room_id, lamport,
  event_id)` composite for `resume_from`; `(occurred_at_ms)` for
  time-range scans; `(ttl_expires_at)` for beacon drains.
- **Pool sizing**: SQLite is single-writer — `max_connections(1)`
  on write side; open read-only connections separately with
  `PRAGMA query_only=ON`. Postgres pool = `(cpu × 2) + spare`.
- **Batching**: `insert_many()` for high-event-rate workloads
  (≥100 events per flush). Wrap in a transaction.
- **JSON columns**: keep `body`/`metadata` JSON, but **don't**
  store anything you need to query (peer_id, room_id, etc.) as
  JSON — promote those to typed columns with indexes.
- **WAL mode**: `PRAGMA journal_mode=WAL` (already on events) —
  apply to all tables. Allows concurrent readers during write.

### Benchmarks to land BEFORE refactoring

Need numbers we can defend, not vibes. Five baselines to measure
on the current code, then prove wins:

1. **Throughput**: `events/sec from 1 publisher → 1 subscriber
   with 0/5/50 broadcast consumers`. Target: 5000 ev/sec at <500 µs
   p99.
2. **Peer registry lookup at scale**: `peer_id verification at
   N=10/100/500 peers under 100 concurrent frame tasks`. Target:
   <5 µs at N=500.
3. **Subscription join latency**: `cold first join + 100th join on
   same wire`. Target: <10 ms cold, <2 ms warm.
4. **Broadcast lag**: histogram from `append_sent_frame` →
   subscriber's first read. Target: <100 µs p99.
5. **Allocation churn**: heap profile at 1000 ev/sec for 10 s.
   Target: sub-linear RSS growth, <10 allocs/event after warmup.

Bench harness lives in `crates/airc-lib/benches/` (new). Criterion
or `divan`. Numbers go in `docs/architecture/PERF-BASELINES.md`
(new).

## Phase 4 — Command Bus Primitive

Goal: support Continuum/Hermes/OpenClaw command traffic without each
consumer reinventing request/reply matching.

This is not a domain command vocabulary. It is a substrate primitive
for correlation, reply matching, deadlines, and cancellation.

Work:

- Add envelope/header conventions for:
  - `airc.correlation_id`
  - `airc.reply_to`
  - `airc.deadline_ms`
  - `airc.command_kind`
- Add `airc-lib` helpers:

  ```rust
  Airc::request(target, headers, body, deadline) -> PendingCommand
  Airc::reply(reply_to, headers, body)
  Airc::cancel(correlation_id)
  Airc::await_reply(correlation_id, timeout)
  ```

- Keep payload semantics consumer-owned:
  - Continuum may define `continuum.lora.invoke`.
  - Hermes may define `forge.hermes.agent_command`.
  - OpenClaw may define UI/user commands.
  - AIRC only handles correlation and delivery.

Acceptance gates:

- A test consumer can issue a command, receive a reply, and replay the
  transcript by correlation ID.
- Timeout and cancellation produce typed events.
- The primitive can carry Continuum's command-bus contract without
  importing Continuum code.

## Phase 5 — Consumer Proofs

Goal: prove this is a grid substrate, not only an agent chat tool.

Required proofs:

- Codex + Claude: two agents coordinate with `airc join` and `airc msg`
  without paste relay.
- Continuum fixture: room/event bus consumes AIRC events and commands
  through `airc-lib`.
- OpenClaw fixture: user/chat surface maps rooms and presence through
  typed subscription APIs.
- Hermes fixture: command orchestration sends request/reply traffic
  through the command primitive.
- Cross-machine fixture: GitHub/gist is used only to publish registry
  metadata; runtime traffic takes local/LAN/tailnet/relay routes.

Proof order:

1. Same-machine Codex/Claude/Continuum/OpenClaw/Hermes shape.
2. Tailscale or LAN multi-machine proof under one operator account.
3. Multi-human grid-to-grid proof after the local/tailnet path is
   stable.

Acceptance gates:

- No proof parses `airc` human prose.
- No proof depends on GitHub for routine same-machine traffic.
- Every proof has replay data suitable for debugging without the
  original UI or runtime process.
- Per non-negotiable #10, proofs emulate the production shape they
  are validating. A "setup-only" join test still has to prove
  setup; a "monitor stream" test still has to prove the stream.
  Disable-flags (`AIRC_NO_ATTACH`, `AIRC_DISABLE_ACCOUNT_REGISTRY`,
  etc.) are admissible only when the proof is explicitly about
  the disabled-path behavior.

### Known emulation gaps (Phase 5 follow-ups)

These track places where CI disabled a production behavior instead
of emulating it. Open items are substrate bugs to file once the
companion phase work lands; closed items stay here as regression
context.

- **Closed: e2e join harness no longer sets `AIRC_NO_ATTACH=1` for
  setup-only subprocesses.** It now emulates an agent runtime by
  removing Cargo harness markers, setting a stable runtime client,
  reading stdout until the attach marker, terminating the child, and
  asserting on the captured stream. This proves the actual attach +
  stream path instead of only the setup return.
- **Closed for LAN-shaped command traffic: cross-machine fixture is
  emulated under CI with separate homes and no shared local-fs data
  plane.** `airc-lib` now proves command-bus request/reply over the
  LAN-TCP route (`request_and_reply_round_trip_over_lan_without_github`).
- **Closed for relay-shaped command traffic: non-LAN fixture is
  emulated under CI with separate homes, a real `airc-relay` server,
  and no GitHub/shared-fs data plane.** `airc-lib` now executes the
  Relay route directly and proves command-bus request/reply over it
  (`request_and_reply_round_trip_over_relay_without_github_or_lan`).
- **Closed for consumer-shaped command traffic: Continuum, OpenClaw,
  and Hermes contract payloads now ride the command-bus request/reply
  primitive over LAN-TCP with separate homes.** The
  `consumer-shapes` fixture proves typed downstream payloads can use
  `Airc::request`, `Airc::reply`, and `Airc::await_reply` without
  parsing CLI prose or depending on GitHub/shared-fs runtime traffic
  (`consumer_contracts_round_trip_over_lan_command_bus`).
  Remaining broader proof: tailnet/relay across real machines before
  promoting the rewrite beyond local/LAN/relay confidence.

## Immediate PR Queue

1. Add the git/PR/kanban event adapter skeleton so agents can
   subscribe to work-state changes instead of polling.
2. Add real-machine tailnet/relay proof that exercises the same route
   execution across host boundaries without GitHub routine traffic.
3. Bind the same command-bus request/reply proof in real Continuum,
   OpenClaw, and Hermes repos once their adapters depend on `airc-lib`.

Done or superseded:

- Store-backed runtime cursors are now in place; `airc join` and Codex hook
  consumers never replay the whole backlog and never write cursor JSON
  sidecars.
- Codex feed contract is documented: `airc join` is the live feed,
  `airc codex-hook poll --wait-ms N` is the bounded mid-turn feed,
  UserPromptSubmit is catch-up, and true wake-on-AIRC requires Codex
  runtime support.
- Runtime context detection is isolated in `airc-cli::runtime_context`;
  `commands.rs` no longer owns join stream/exit heuristics.
- Codex hook/feed/config/start files live under
  `airc-cli::integrations::codex`; the top-level CLI no longer owns
  Codex-specific implementation modules.
- Subscription query APIs are in `airc-lib`: consumers can inspect
  joined channels, default room, and cursors without parsing CLI prose.
- Command-bus request/reply helpers are in `airc-lib`, with directed
  envelope targets and correlation/deadline headers.
- Command-bus request/reply is proven over LAN-TCP with separate homes
  and no GitHub/shared-fs data plane.
- The e2e join harness now emulates a Monitor-shaped streaming
  consumer instead of disabling attach with `AIRC_NO_ATTACH`.

## Open Questions

1. Should `RuntimeContext` live in `airc-lib` or a new
   `airc-integrations-common` crate?
2. Which lifecycle events does Continuum need first for its room and
   persona event bus?
3. Should the command-bus primitive use only headers for correlation,
   or mirror correlation in body for consumer convenience?
4. What is the minimum cross-machine proof before promoting
   `rust-rewrite` toward canary?
