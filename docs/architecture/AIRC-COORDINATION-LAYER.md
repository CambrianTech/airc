# AIRC — The Coordination Layer

**Status:** canonical top-level architecture for `rust-rewrite`. Designed top-down.
**Date:** 2026-05-26
**Supersedes nothing; consolidates:** this is the entry-point model. The existing
docs elaborate specific layers and remain binding for their detail:
[`realtime-event-bus.md`](../realtime-event-bus.md) (delivery/event detail),
[`ACCOUNT-MESH-JOIN-CONTRACT.md`](ACCOUNT-MESH-JOIN-CONTRACT.md) (identity/join),
[`rust-substrate-grievances-and-gaps.md`](../rust-substrate-grievances-and-gaps.md)
(control board, work-coordination domain).

## 0. Purpose (the top)

AIRC is **the common denominator**: one coordination layer through which many
systems (Continuum, Claude Code, Codex, OpenClaw, Hermes) and many agents
(human, AI persona, service daemon) integrate — for **communication and
co-development**. It is not a chat tool. It is the bus that:

- lets agents and personas **talk** (chats, rooms, DMs, presence);
- lets them **coordinate work** (kanban cards, lanes, claims, worktrees);
- lets them **observe and replay** everything (events, subscriptions, cursors);
- does this **across many threads and many users all converging** on shared
  rooms, on one machine and across machines.

Everything built on AIRC inherits its properties. So the non-negotiable
property is **reliability/determinism**: a coordination layer that is
intermittently wrong poisons every system and agent above it. Flakes, leaks,
and races are not minor — they are the layer failing at its one job.

## 1. Design principles (design up, not up-from-the-bug)

1. **Design from purpose down to the model**, not up from individual bugs.
   Flakes/leaks/races are symptoms of a missing top-level model; fix the model,
   not the symptom.
2. **Reliability is structural.** The architecture must make whole classes of
   failure *impossible by construction*, not patched after the fact.
3. **One model, many domains.** Chat, rooms, presence, kanban, worktrees are all
   the *same* primitive — a typed event on a channel, delivered to subscribers.
4. **Ephemeral sessions, durable substrate.** Agents/tabs/personas come and go
   constantly; that must be a no-op for everyone else.
5. **Determinism = testability.** If state has one owner and clients are
   isolated sessions, tests run against isolated instances with explicit
   lifecycle — there is no shared global state to race.
6. **Uniform consumer surface.** Continuum personas and Claude agents use the
   *same* client API and the *same* typed events. No per-consumer hacks in core.

## 2. The model (each layer derives the next; reliability falls out)

### 2.1 Ownership — one owner of state per machine account
**Decision (2026-05-26):** exactly one long-lived **machine-account daemon**
owns the ORM (`~/.airc/events.sqlite`) and all coordination state. Nothing is
shared through ad-hoc globals — no `frames.jsonl` polled by N processes, no
racing `/tmp` sockets, no real `~/.airc` touched directly by tests.

*Eliminates by construction:* the N-daemon-per-scope model, the leaked-daemon
class (21 orphaned daemons observed from test runs), the file-bus poll latency,
and the cross-process file-sync bug that was the root of "two tabs can't talk."

### 2.2 Sessions — clients attach, ephemeral
Every consumer (a Claude tab, a Codex run, a Continuum persona, a service) is a
**session/client** that attaches to the owner over one typed IPC/API. Sessions
are ephemeral; the substrate is durable. Opening or closing a session is a no-op
for every other participant — no election, no re-pair, no dropped message.

*Eliminates by construction:* the "tabs online/offline churn the mesh" class;
host re-election thrash; identity/pairing corruption on reconnect.

### 2.3 Identity & rooms — account-canonical, deterministic
Identity is resolved **once per machine account** (the gh account user is the
canonical anchor; weaker sources upgrade to it) and shared via the owner.
`room_id = derive(account_identity, channel_name)` is then **deterministic and
convergent by construction** — every scope/agent on the account lands on the
same room. (#1014 fixed the same-machine case; cross-machine convergence is the
account-registry path, still a gap.)

*Eliminates by construction:* the per-scope identity divergence that fractured
`#cambriantech` into two invisible rooms.

### 2.4 One event primitive
Everything on the bus is a typed event:

```
Envelope { event_id, sender (peer+client), channel (room_id), target,
           lamport + occurred_at, headers, body (typed payload), delivery_class }
```

- **`DeliveryClass`**: `Durable` (chat, claims, lifecycle — persisted, replayable),
  `EphemeralLatest` (presence/typing — coalesced with TTL), `EphemeralWindow`,
  `RequestResponse` (correlated), `StreamChunk`.
- **`PayloadFamily`**: `AircNative` plus per-consumer families (Continuum
  JTAG/EventBridge/GridFrame/LiveKit, …) carried unchanged via a `SchemaAdapter`
  boundary. AIRC routes on headers and delivery class; it never reinterprets a
  consumer's payload semantics.
- **Subscriptions** filter by channel/kind/headers with durable per-consumer
  cursors (replay-from-cursor + live push in one contract).

### 2.5 Domains are typed events (one bus, not bolted-on subsystems)
**Decision (2026-05-26):** kanban, worktrees, chat, presence are **typed
payloads on the one event primitive**, not separate subsystems.

| Domain | Event payloads (typed) | DeliveryClass |
|---|---|---|
| Chat / rooms | message, attachment, receipt | Durable |
| Presence | arrived/departed, typing, ready/away | EphemeralLatest |
| Kanban | `WorkCard{Created,Claimed,Released}`, `Lane*`, `ManagerHat*` | Durable |
| Worktrees | `Workspace{Requested,Allocated,Heartbeat,Released}` leases | Durable |
| Lifecycle | RoomJoined/Parted, WireEstablished, SubscriptionAdvanced | Durable |
| Realtime ctl | WebRTC/LiveKit offer/answer/ICE *metadata* (not media) | EphemeralWindow |

`airc-work` already models kanban/worktrees as typed events + projections. The
work is to make them ride this unified bus and be consumed through the *same*
client API as chat — so a board view, a room, and a worktree roster are all just
subscriptions with filters.

### 2.6 Delivery & performance
Same-machine delivery is **in-process / IPC push fan-out** from the owner —
sub-millisecond, one signature verify per message (not N), no polling. This is
what carries many threads + many users converging (14 personas + UI + agents).
Cross-machine sync uses real transports (LAN/Tailscale/relay/WebRTC); GitHub is
registry/bootstrap only, never the routine data plane. The current adaptive
file-poll is an explicit **stopgap** the owner-push model retires.

Perf is **gated, not claimed**: p95 publish→subscriber < 20 ms; 4/8/16 (and 14)
concurrent subscribers; idle subscription < 1% CPU; replay benchmarks. See
`crates/airc-lib/tests/fanout_bench.rs`.

### 2.7 Determinism & testability (reliability as a first-class invariant)
Because state has one owner and clients are isolated sessions, **every test runs
against an isolated, embeddable owner instance with explicit lifecycle** — no
process-global env, no shared `~/.airc`, no `/tmp` socket collision, every
spawned daemon reaped. The test model *mirrors* the production model
(`airc-lib` is embeddable; `open_with_wire_root_for_test` gives explicit isolated
state). A flaky test is the design telling us a test still leans on shared global
state — the fix is to remove the shared state, not to rerun.

## 3. Reliability invariants (must hold; the model enforces them)

- **No shared mutable global state** in the runtime or tests (no global env
  mutation, no shared real `~/.airc` in tests, no orphaned processes).
- **Convergence is deterministic** — same account ⇒ same room_id, always.
- **Session lifecycle is a no-op** for other participants.
- **No message lost on the durable path; ephemeral drops are counted, not
  silent.**
- **Every CI job green before merge** — this repo does not enforce required
  checks, so the merging agent verifies *every* job (esp. `windows-latest`); no
  `--auto`. (Learned the hard way: #1014 merged red on Windows; #1016 repaired.)
- **Tests reap what they spawn.** A test that leaks a daemon or a temp home is a
  bug in the test.

## 4. Consumer integration (the payoff)

All consumers attach the same way and speak the same typed events:

- **Claude Code / Codex agents** develop *through* AIRC: claim kanban cards, spin
  worktrees per claim, coordinate in rooms, emit/subscribe to events. AIRC is the
  medium of multi-agent co-development, not a tool beside it.
- **Continuum** embeds `airc-lib` directly; its personas are in-process
  subscriptions over the owner's fan-out (sub-µs), carrying JTAG/GridFrame/LiveKit
  payloads unchanged via `SchemaAdapter`. Continuum enables local development on
  top of this.
- **Hermes / OpenClaw / future grids** attach as sessions with their own payload
  families. No consumer-specific logic in AIRC core.

## 5. Where we are vs. this model (honest gap map)

Landed (rust-rewrite): event/subscription store (`airc-store`), `airc-lib`
embeddable facade, `airc-work` typed kanban/worktree model + projections, daemon
+ IPC v4, account-canonical same-machine identity (#1014), deliver-first +
adaptive-poll delivery (#1015, the poll is stopgap), test isolation via explicit
wire roots (#1016).

Gaps to close, top-down (each a clean, benchmark+reliability-gated slice):
1. **Owner consolidation** — collapse to one machine-account daemon; clients
   attach as sessions; retire the file-bus poll and the N-daemon/leaked-process
   class. *(Biggest reliability + perf win; also fixes the macOS test flake class
   structurally.)*
2. **Unified typed delivery** — `DeliveryClass` + presence coalescing
   (`realtime_latest`); promote chat off untyped `json{text}` to a typed family.
3. **Domains on the one bus** — `airc-work` kanban/worktrees consumed through the
   same client API/subscriptions as chat.
4. **SchemaAdapter + PayloadFamily** — carry Continuum/Hermes payloads unchanged.
5. **Cross-machine convergence** — account-registry publish/refresh; same account
   converges across machines without a pasted invite.
6. **Continuum embed proof** (Gate 4/6) — personas as in-process subscriptions.

Test-hygiene pass runs alongside #1: every spawned daemon reaped, every test on
isolated state, no global env — so reliability is structural, not chased.

## 6. Non-negotiables (carried from the grievances control board)

No silent fallback to slow/insecure paths. No hard-coded machine paths. No
consumer-specific semantics in core. No CLI-text parsing as the integration API.
No raw SQL in consumers. Every production behavior recordable + replayable. Every
perf claim has a reproducible measurement. Every cross-machine claim has a
cross-machine test. **And: the development process on AIRC must be trustworthy
enough that agents build on it without babysitting.**
