# Cambrian Consumer Integration Matrix

**Status**: Map of which Cambrian projects plug into airc and how.
The substrate is generic — what makes it Cambrian-shaped is the
set of consumers riding on it. This doc is the canonical roster.

If you're adding a new consumer, add a row + integration README.

## Why this exists

airc is the backbone bus for all Cambrian work coordination — AR,
distributed inference, agent workflows, tool chains, fleet
operations. Each consumer plugs in differently: some are pure
event subscribers, some publish presence, some need request/reply,
some run on the data plane at AR cadence. This matrix is the
single read for "who uses what."

## Matrix

| Consumer | Role | Profile | Headers used | Tables read | Tables written | Integration README |
|---|---|---|---|---|---|---|
| **Continuum** | AR pose + spatial sync, persona event bus, distributed inference orchestration | A (pose) + B (anchors) + C (commands) | `forge.persona.*`, `continuum.lora.invoke`, `airc.route_class` (TBD) | `events`, `subscriptions`, `peer_trust`, `beacons` | `events` (broadcasts), `subscriptions` (joins) | `integrations/continuum/README.md` |
| **OpenClaw** | Workspace UI surface (threads, users, presence, render targets) | C (event-driven UI) | `openclaw.thread.*`, `openclaw.user.*` | `events`, `subscriptions`, `peer_trust`, `local_identity` | `events`, `subscriptions` | `integrations/openclaw/README.md` |
| **Hermes** | Command/orchestration plane for agent workflows | C (request/reply) | `forge.hermes.agent_command`, `airc.correlation_id` (TBD Phase 4) | `events`, `peer_trust` | `events` | `integrations/hermes/README.md` |
| **Claude Code** | AI agent runtime (Monitor stream + skill bindings) | C (chat + commands) | `airc.client=claude:*`, `airc.correlation_id` (TBD) | `events`, `subscriptions`, `runtime_cursors`, `peer_trust`, `local_identity` | `events`, `subscriptions`, `runtime_cursors` | `integrations/claude-code/README.md` |
| **OpenAI Codex** | AI agent runtime (UserPromptSubmit hook + persistent join session) | C (chat + commands) | `airc.client=codex:*`, `airc.correlation_id` (TBD) | `events`, `subscriptions`, `runtime_cursors`, `peer_trust`, `local_identity` | `events`, `subscriptions`, `runtime_cursors` | `integrations/openai-codex/README.md` |
| **Cursor / opencode / Windsurf** | IDE-resident agent surfaces | C | `airc.client=<runtime>:*` | same as Claude/Codex | same as Claude/Codex | placeholders in `integrations/`; READMEs missing until those integrations ship |
| **agent-relay** | Cross-runtime coordination broker (Cambrian internal) | TBD — scope: bridge between heterogeneous agent runtimes? | TBD | TBD | TBD | TODO |
| **forge-alloy** | Cambrian internal — see Cambrian workspace | TBD | TBD | TBD | TBD | TODO |
| **sentinel-ai** | Cambrian internal monitoring/alerting | C (events) | `sentinel.alert.*` (TBD) | `events` | possibly `events` for alert publish | TODO |
| **Generic** | Reference IDE/CLI consumer for protocol stability | C | `airc.client=generic:*` | demonstrative | demonstrative | `integrations/generic/README.md` |

## Headers convention

The substrate routes on headers; it does not interpret payloads.
Consumers should follow:

- `airc.client` — runtime tag, e.g. `claude:<session>`, `codex:<thread>`, `continuum:<scope>`.
  Used for self-filter on shared HOME (post-#869).
- `airc.correlation_id` (Phase 4) — request/reply correlation. Set
  by request emitter, echoed by reply.
- `airc.reply_to` (Phase 4) — where to direct the reply (usually
  the requesting peer_id; can be a different channel).
- `airc.deadline_ms` (Phase 4) — request expiry; receivers may drop
  past-deadline requests.
- `airc.route_class` (proposed, AR) — `local-only` / `lan-allowed`
  / `any`. Resolver respects.
- `airc.priority` (proposed, AR) — `low` / `default` / `high`.

Consumer-specific headers use a consumer-prefixed namespace:
`forge.persona.*`, `openclaw.thread.*`, `continuum.lora.*`. The
substrate sees them as opaque routing keys.

## Table-access conventions

Per non-negotiable #6 ("Consumer integrations must be thin"),
consumers SHOULD:

- Read state through `airc-lib` typed APIs (`Airc::peers`,
  `Airc::subscriptions`, etc.), not direct SQL.
- Write state through `airc.say` / `airc.send` and the typed
  primitives that emit events, not direct table inserts.

Consumers SHOULD NOT:

- Open `events.sqlite` for direct read — the schema is internal,
  not API-stable.
- Modify any table — substrate-side invariants depend on
  controlled-write paths.
- Reach into wire files (`<wire-root>/wires/<channel>/frames.jsonl`)
  unless they have explicit substrate-team approval.

The exception: tooling docs / `airc doctor` / debug tools may read
from the ORM directly for inspection. Production consumers go
through the SDK.

## Per-consumer onboarding

Each consumer's integration README should answer:

1. Which subset of the matrix above (headers / tables) applies.
2. The lifecycle hooks the consumer needs (presence updates,
   peer joins, room subscriptions changing).
3. The failure modes the consumer must handle (peer not in
   registry — see #905 skip-and-warn; route degraded — see
   transport health; persistent failure — see
   `airc doctor`).
4. The acceptable cadence / payload-size envelope (see
   [AR-LATENCY-CONTRACT.md](AR-LATENCY-CONTRACT.md) for Profile A/B/C).

## Gaps and TODOs

These consumers don't have integration READMEs yet. Each blocks a
production deployment of that consumer:

- **agent-relay** — Cambrian-internal scope unknown to substrate
  docs. Owner: Cambrian team. Needs the matrix row above filled in.
- **forge-alloy** — same.
- **sentinel-ai** — referenced as monitoring/alerting; need scope
  doc for alert publish, ack contract.
- **Cursor / opencode / Windsurf** — listed in `README.md` IDE
  table but no READMEs. Should land before each shipping
  integration.

## Adding a new consumer

1. Pick a vocabulary prefix (e.g. `myproject.*`). Document it in
   the integration README.
2. Decide your profile (A AR / B anchors / C events). Set
   `airc.route_class` + `airc.priority` if not Profile C.
3. Stamp `airc.client=<runtime>:<instance>` on every send so the
   self-filter works on shared HOME.
4. Subscribe via `Airc::subscribe_subscribed_filtered(filter)` —
   the multi-room surface, not the current-room narrow one.
5. Use `runtime_cursors` for resumable replay; don't invent a
   sidecar.
6. Add a row to the matrix above; add `integrations/<name>/README.md`.

## See also

- [GRID-SUBSTRATE-AUDIT.md](GRID-SUBSTRATE-AUDIT.md) — substrate
  doctrine, non-negotiables, phase progress.
- [AR-LATENCY-CONTRACT.md](AR-LATENCY-CONTRACT.md) — cadence /
  latency budgets for AR consumers (Profile A/B).
- [DATA-MODEL-REFERENCE.md](../DATA-MODEL-REFERENCE.md) — table
  schemas for the read/write columns above.
