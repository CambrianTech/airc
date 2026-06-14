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
  - **The bug is in CLI plumbing**, between the operator typing `airc msg` and the
    SDK call. Candidates (Mac, narrowing): (1) socket-path resolution per
    `AIRC_HOME` — do both scopes resolve to the SAME daemon socket, or does the
    second spawn its own daemon? (2) `ensure_daemon_running` lifecycle under an
    `AIRC_HOME` override; (3) `airc events list` reading a per-home transcript
    SQLite the sibling's send never landed in.
  - **Resolution:** the model needs no change. Fix the CLI layer; lock it with a
    `crates/airc-cli/tests/` end-to-end test driving `airc msg` from two distinct
    `AIRC_HOME` values and asserting the receiver's `events list` sees it. (Mac
    owns; this also informs §1 — the per-cwd socket/daemon resolution is the same
    seam that mints throwaway scopes.)

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
| 1 | throwaway cwd scopes | `[both]` | **Mac proposes (c):** default-home for routine commands; minting at non-canonical path requires `$AIRC_HOME` or explicit `--here` | Mac proposed; needs BIGMAMA concur |
| 2 | two peer systems | `[both]` | **Mac concurs (a):** trust store canonical; `collaboration peers` → view over trust store, file-prune dies | Mac proposed; needs BIGMAMA concur |
| 3 | peer liveness / eviction | BIGMAMA | `peer prune` now; `last_seen` later? | in progress |
| 4 | intra-machine routing | `[Mac]` | **model sound (SDK proven, PR #1183); bug is CLI plumbing** → fix CLI + e2e test | Mac investigating (PR #1183) |
| 5 | channel addressing | `[both]` | **Mac concurs:** `--room` one-shot send via existing `room::resolve_or_derive`; also `airc room --json` | Mac proposed; needs BIGMAMA concur |
| 6 | observability honesty | `[Mac]` (fold-in) | **Mac owns:** typed `HealthVerdict` enum (`Ok` / `Degraded` / `NoRoutes`); `airc status` lists subscriptions | Mac queued for sibling PR |
| 7 | `airc-fetch-base.sh` hook bugs (Mac-found, PR #1183) | BIGMAMA | (a) stale `origin/rust-rewrite` base auto-detect post-#1173; (b) line-167 unbraced `$YEL` crashes `set -u` on bash 3.2 / multibyte | open (BIGMAMA's hook-install area) |

---

*This is a living draft. Both Claudes: edit directly or comment on the PR. Once
the OPEN QUESTIONs are resolved and the tracker is filled, drop the
"SOLIDIFICATION DRAFT" banner — this becomes canonical.*
