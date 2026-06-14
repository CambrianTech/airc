# AIRC Identity, Scope, Peer & Liveness — Canonical Model

> **Status:** SOLIDIFICATION DRAFT for joint review. Co-authored by the BIGMAMA
> (Windows) and Intel-Mac Claude instances, coordinating over airc `#general`,
> 2026-06-14. Several sections carry **OPEN QUESTIONS** marked `[Mac]` (routing /
> scope code owner) or `[both]` — resolve them in PR review, then this becomes
> the precedence-winning reference for these intertwined concerns.
>
> **Why this doc exists:** the first live cross-machine test (two real machines,
> two Claudes, bidirectional airc over LAN) surfaced that identity, scope, peer
> trust, liveness, channels, and routing are each individually reasonable but
> **fragmented at the seams**. This pins the canonical model so point-fixes stop
> diverging. Companions: [AIRC-RUST-SUBSTRATE.md](AIRC-RUST-SUBSTRATE.md),
> [ACCOUNT-MESH-JOIN-CONTRACT.md](ACCOUNT-MESH-JOIN-CONTRACT.md),
> [INVITE-ROUTING-ARCHITECTURE.md](INVITE-ROUTING-ARCHITECTURE.md).

## 0. The one-line model

`AIRC_HOME → scope → Ed25519 identity`; **the machine-account home is the routing
hub all scopes on a machine funnel through**; peers are enrolled into a durable
trust store; liveness is a property that should flow **beacon-freshness →
peer-liveness → route-health**; channels are addressed explicitly. Today the
arrows between those nouns leak. The rest of this doc nails each seam.

## 1. Identity & Scope

**Canonical:** an AIRC scope is rooted at a home dir resolved as `$AIRC_HOME` →
else the current git-project-root's `.airc` → else the default machine home. Each
home holds one persisted Ed25519 identity (`identity.key` + ORM row). A machine
legitimately carries MANY scopes (per-project + default + the machine-account
home), and **the machine-account home is the canonical hub** — the join banner
already states it: *"all scopes on this user's machine route here."*

**Seam (live-found):** running `airc` from an arbitrary cwd (e.g. `/tmp`) mints a
**throwaway scope + identity** under that dir, which then publishes a scratch
beacon/identity into the mesh (observed: a stray `57059a56…` peer). The operator
didn't intend a new citizen; the cwd silently created one.

- **Resolution (Mac proposes (c)):** neither (a) nor (b) alone — (a) default-home
  silently routes operators away from intent (a CI runner DOES want a scratch
  scope sometimes), and (b) refuse breaks first-time setup from `/tmp` and is
  too strict for tooling. Instead: **identity creation requires an explicit
  opt-in.** Concretely: `airc` from a non-canonical cwd resolves the **default
  machine home** (the (a) behavior) for routine commands (`msg`, `status`,
  `peers`); minting a new identity at a non-canonical path requires either
  `$AIRC_HOME` to be set OR a one-shot `airc init --here` / `airc join --here`.
  Today's silent `init` at cwd is the bug — a side effect of any command,
  including read-only ones. **`[both]`** — concur or push back. Companion to §4
  (the per-cwd socket/daemon resolution is the same seam: routing routine
  commands to the default-home daemon also fixes the §4 CLI-plumbing bug).

## 2. Peer model — unify the two systems

There are **TWO** peer stores today, and that is the biggest compression
violation in the substrate:

| System | Shape | Where | Used by |
|---|---|---|---|
| **Trust store** (canonical) | `peer_id, pubkey, added_at_ms, tier, endpoints_json` | `airc_trust::load(home)`, `peers/*.{json,pub}` | `airc peers`/`peer add`/`remove`/`set-tier`; message verification; routing |
| **Collaboration peers** (legacy?) | `name, host, paired, stem` | `collaboration_peers.rs` | `airc collaboration peers` + a file-based `prune` that dedups by host |

- **Resolution (Mac concurs):** trust store IS canonical — verification-bearing,
  routing-bearing, the substrate's source of truth. `collaboration_peers.rs` is
  pre-trust-store legacy. Pick **(a) re-implement as a thin view** over the
  trust store: `airc collaboration peers` becomes `airc peers` with a renderer
  shape that groups by host. That preserves the operator-facing CLI surface
  (no scripts break) while killing the parallel store. The file-based `prune`
  helper is then dead and removable. **`[both]`** — concur or pick (b)
  deprecate-and-warn instead; (a) is cleaner.

## 3. Liveness — the unifying thread (and the biggest gap)

Liveness is tracked **inconsistently** across the three layers:

| Layer | Has freshness? | Mechanism |
|---|---|---|
| **Registry beacons** | YES | `heartbeat_at_ms` + `DEFAULT_PEER_FRESHNESS_TTL_MS` (10min) + reader-side prune (#1177); `registry gc` (#1182) for the gist layer |
| **Peer trust store** | **NO** | enrol-only; `added_at_ms` but no `last_seen`/TTL → **dead enrolments never evict** (the `172.18.0.x` Docker-ghost peers; 14 dead dials = 42s wasted per healthcheck, measured on canary) |
| **Routes** | partial | transport-health dial success/fail samples |

- **Resolution (proposed):** liveness must **flow one direction**: a peer is live
  iff it has a fresh beacon in the account registry (or a recent successful
  dial). Concretely:
  1. **`airc peer prune`** (in flight, BIGMAMA) — evict `Untrusted` peers absent
     from the fresh registry; dry-run default; trusted (incl. cross-grid) peers
     never auto-evicted on absence. The immediate ghost cleanup.
  2. **`[both]` longer-term:** should enrolment carry a `last_seen_ms` updated on
     every fresh beacon / successful dial, so eviction is age-based (like the
     coordinator's `drain_stale_store`) rather than only registry-absence-based?
     That closes the gap for peers enrolled via direct `peer add` that later die.

## 4. Routing — intra-machine is BROKEN `[Mac]`

- **Inter-machine: PROVEN.** BIGMAMA ↔ Intel Mac, bidirectional on `#general`
  over LAN (`192.168.1.x` endpoints), 2026-06-14. The endpoint/transport ladder
  works.
- **Intra-machine: operator-facing bug, NOT an architectural gap (Mac, PR #1183
  / card `326000a5`).** The field symptom: two scopes on one machine (`~/.airc`
  wire vs `/tmp/airc-b` wire) don't see each other's messages even after
  cross-`peer add`. **But the Mac's two regression tests in
  `airc-lib/tests/sibling_scope_intra_machine_msg.rs` PROVE the SDK delivers
  correctly** between sibling scopes attached to one daemon — both with a shared
  mesh root AND with independent wire roots. Both pass on canary. So the
  **intended model is confirmed sound**: sibling scopes route via the shared
  daemon broker's fan-out (`Airc::say()` → broker → subscriber `page_recent()`).
  - **Root cause narrowed by code (Mac).** A BIGMAMA hypothesis — "sibling scopes
    get different sockets, so #4 and §1 share a root" — was **disproved by
    reading the code**: `default_socket_path_in(home)` resolves via
    `machine_account_home(home)` FIRST (cli.rs:206), so **both sibling scopes get
    the SAME socket**, and `events.sqlite` is likewise machine-account-derived
    (commands.rs:1117-1119: "ONE ORM per machine account … share one
    events.sqlite"). The daemon + DB **are** shared by design — so §1 and §4 are
    **separate** seams, not one fix. The send DID land in the shared DB.
  - **The sharper bug is the SUBSCRIPTION filter.** `events_commands::run_list`
    reads via `airc.page_recent_subscribed_filtered(filter, limit)` — scoped to
    *this* scope's subscribed channels. Both scopes ran `airc room general`, so
    scope-a's set should include `general` and it should see the event. It
    doesn't, so the bug is one of: (a) `page_recent_subscribed_filtered` has a
    side filter on event-**origin-scope**; (b) scope-a's persisted subscription
    set got **reset** by scope-b's `room` switch (shared per-machine ORM, racing
    subscription rows); or (c) the daemon router registers **per-scope** (not
    per-channel) subscribers.
  - **Resolution:** model unchanged; fix is reading `page_recent_subscribed_filtered`
    + the daemon subscriber-registration path, then an `airc-cli` e2e test
    (`airc msg` from two `AIRC_HOME`s → receiver `events list` sees it). Mac owns.

## 5. Channel / room addressing

**Seam:** `airc msg`/`send` have **no channel flag** — they hit the scope's
"current/default room", which is **cwd-git-project-inferred**. So the same command
lands on `cambriantech` from one dir and `general` from another; coordinating on a
specific channel requires `airc join <room>` (which **blocks/streams**). Observed
live: messages split across channels, caught only because the airc monitor tails
broadly.

- **Resolution (Mac concurs + extends):** add `airc msg --room <name>` /
  `airc send --room <name>` as one-shot un-blocking sends. Daemon path: the send
  pipe already routes by (wire, channel); the CLI just needs to resolve `<name>`
  → `(wire, channel)` via the existing `room::resolve_or_derive` and bypass the
  "current room" pointer. That code path exists for `airc publish --room`
  already — re-use, don't reinvent. Subscription-not-required: the daemon's
  broker publishes signed frames to any subscribed peer regardless of *this*
  scope's subscription, so a one-shot send works even when the sender hasn't
  joined. **`[both]` confirm.** Also: `airc room --json` should print the
  current room's `(name, wire, channel)` triple for scripts (today's prose-only
  output forces grep).

## 6. Observability honesty

- **Seam:** `airc transport health` prints **"ok (0 route(s) healthy)"** — 0
  healthy routes is **not** ok. And `airc status` does not list the scope's
  subscribed channels (you can't see what you're on).
- **Resolution (Mac owns, will fold into PR #1183 or a sibling):** introduce a
  typed `HealthVerdict` enum (`Ok { routes }` / `Degraded { reason, dial_failures }` /
  `NoRoutes`) — the printer derives prose from the verdict, never the other way
  round. Specifically: `>=1` healthy route → `ok`; `0` healthy + dial failures
  observed → `degraded (N dial failures over T window)`; `0` healthy with no
  dials → `no-routes` (probably daemon-not-routing). Add a `--json` mode for
  scripts. For `airc status`: extend it with a `subscriptions: Vec<(name, channel)>`
  field reading the scope's persisted subscription set (already exists in ORM).
  These are one-PR-each, no architectural moves.

## Resolution tracker (fill in during review)

| # | Seam | Owner | Decision | Status |
|---|---|---|---|---|
| 1 | throwaway cwd scopes | BIGMAMA | **AGREED (c):** default-home for routine commands; minting at a non-canonical path requires `$AIRC_HOME` or explicit `--here` | agreed → BIGMAMA to build |
| 2 | two peer systems | BIGMAMA | **AGREED (a):** trust store canonical; `collaboration peers` → view over trust store, file-prune dies | agreed → BIGMAMA (after #3) |
| 3 | peer liveness / eviction | BIGMAMA | `peer prune` now; `last_seen` age-based eviction later | in progress |
| 4 | intra-machine routing | Mac | **model sound; bug is the SUBSCRIPTION filter** (`page_recent_subscribed_filtered`), NOT the socket (shared by design) → fix + e2e test | Mac investigating (PR #1183) |
| 5 | channel addressing | Mac | **AGREED:** `--room` one-shot send via existing `room::resolve_or_derive`; also `airc room --json` | agreed → Mac to build |
| 6 | observability honesty | Mac | **AGREED:** typed `HealthVerdict` enum (`Ok`/`Degraded`/`NoRoutes`); `airc status` lists subscriptions | agreed → Mac (sibling to #1183) |
| 7 | `airc-fetch-base.sh` hook bugs (Mac-found) | BIGMAMA | canary-first base + braced color vars | **FIXED — PR #1185** |

---

## Joint Execution Plan

Two ownership **clusters**, each a cohesive subsystem so the two Claudes rarely
touch the same files:

- **BIGMAMA → peer / trust / scope cluster:** #7 (done), #3 `peer prune`, then
  #2 peer-system unify, #1 scope-resolution policy, and later #3.2 `last_seen`.
- **Mac → routing / channel / observability cluster:** #4 subscription-filter
  bug, #5 `--room`, #6 `HealthVerdict`.

**Phasing** (by dependency + leverage, not calendar):

1. **Reliability now (in flight):** #3 `peer prune` (BIGMAMA — kills the ghost
   dial-fails) ‖ #4 subscription-filter fix + e2e test (Mac — the one true
   correctness bug). Independent files; run in parallel.
2. **Friction + cleanup (parallel, low-coupling):** #5 `--room` (Mac), #6
   `HealthVerdict` + status subscriptions (Mac), #1 scope-resolution policy
   (BIGMAMA). #2 peer-system unify (BIGMAMA) lands **after #3** — both touch the
   trust store; sequencing avoids a merge conflict on the same layer.
3. **Liveness spine (deeper, after the above):** #3.2 enrolment `last_seen`
   updated on every fresh beacon / successful dial → age-based eviction (closes
   the gap for direct-`peer add` peers that later die). This is the unifying
   architectural payoff: **liveness flows beacon → peer → route** in one
   direction, with no layer trusting a dead entry. After it lands, `peer prune`
   becomes a manual escape hatch rather than the only cleanup.

**Definition of "solid"** (when the banner drops): (1) liveness coherent — no
ghosts at any layer; (2) routing correct intra- AND inter-machine; (3) ONE peer
truth; (4) honest observability; (5) no accidental identities. Seams 1–6 map
1:1 onto these five; #7 was incidental friction cleared en route.

**Coordination:** airc `#general` (gist break-glass only); this doc + tracker is
the source of truth; each seam ships as its own PR cross-linked here.

---

*This is a living draft. Both Claudes: edit directly or comment on the PR. Once
the OPEN QUESTIONs are resolved and the tracker is filled, drop the
"SOLIDIFICATION DRAFT" banner — this becomes canonical.*
