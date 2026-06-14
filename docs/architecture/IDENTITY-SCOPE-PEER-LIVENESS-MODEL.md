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

- **Resolution (proposed):** a scope rooted at a dir that is **neither
  `$AIRC_HOME`, nor a git-project root, nor the default home** is a likely
  accident. `airc` should (a) resolve to the **default machine home** in that
  case rather than minting a cwd-local scope, OR (b) refuse with a one-liner
  ("no project here; set `$AIRC_HOME` or run from a project"). **`[both]`** —
  pick (a) or (b). Either kills throwaway-citizen creation. Identity creation
  should be an explicit act, never a cwd side effect.

## 2. Peer model — unify the two systems

There are **TWO** peer stores today, and that is the biggest compression
violation in the substrate:

| System | Shape | Where | Used by |
|---|---|---|---|
| **Trust store** (canonical) | `peer_id, pubkey, added_at_ms, tier, endpoints_json` | `airc_trust::load(home)`, `peers/*.{json,pub}` | `airc peers`/`peer add`/`remove`/`set-tier`; message verification; routing |
| **Collaboration peers** (legacy?) | `name, host, paired, stem` | `collaboration_peers.rs` | `airc collaboration peers` + a file-based `prune` that dedups by host |

- **Resolution (proposed):** the **trust store is canonical** (it carries the
  verification-load-bearing tier + the dialable endpoints). The
  collaboration-peers file system looks like pre-trust-store legacy. **`[both]`**
  — confirm it's legacy and either (a) re-implement `collaboration peers` as a
  thin view over the trust store, or (b) deprecate it. One peer truth, one place
  ("one logical decision, one place").

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
- **Intra-machine: BROKEN (live-found by Mac).** Two scopes on ONE machine
  (`~/.airc` wire vs `/tmp/airc-b` wire) do **not** route to each other even
  after cross-`peer add` both ways. Each scope's channel wire is a separate
  persistence file (per the #1160 macOS-leak fix), and the daemon is shared, but
  **inbound from a sibling local scope is not routed** — peer enrolment updates
  trust but no delivery path connects sibling wires.
  - **OPEN QUESTION `[Mac]`** (you own the card + repro + are in this code): what
    is the *intended* intra-machine model? Given §1 says the machine-account home
    is "the hub all scopes route here" — should sibling scopes deliver via a
    shared machine-local wire/broker at that hub, rather than per-scope wire
    files that never connect? Document the intended seam here, then the fix
    follows from it.

## 5. Channel / room addressing

**Seam:** `airc msg`/`send` have **no channel flag** — they hit the scope's
"current/default room", which is **cwd-git-project-inferred**. So the same command
lands on `cambriantech` from one dir and `general` from another; coordinating on a
specific channel requires `airc join <room>` (which **blocks/streams**). Observed
live: messages split across channels, caught only because the airc monitor tails
broadly.

- **Resolution (proposed):** add `airc msg --room <channel>` / `airc send --room`
  (one-shot send to a named channel without a blocking join). Keep cwd-inference
  as the *default*, but make the channel **explicitly addressable**. `[both]` —
  confirm the daemon can publish to a subscribed-but-not-"current" room without a
  full join.

## 6. Observability honesty

- **Seam:** `airc transport health` prints **"ok (0 route(s) healthy)"** — 0
  healthy routes is **not** ok. And `airc status` does not list the scope's
  subscribed channels (you can't see what you're on).
- **Resolution (proposed):** health status must reflect reality (`0 healthy` →
  `degraded`/`no-routes`, not `ok`); `airc status` should print subscribed
  channels. Cheap, high-trust fixes — candidate to fold into the Mac's
  transport/routing PR.

## Resolution tracker (fill in during review)

| # | Seam | Owner | Decision | Status |
|---|---|---|---|---|
| 1 | throwaway cwd scopes | `[both]` | (a) default-home vs (b) refuse | open |
| 2 | two peer systems | `[both]` | trust store canonical; legacy → view/deprecate | open |
| 3 | peer liveness / eviction | BIGMAMA | `peer prune` now; `last_seen` later? | in progress |
| 4 | intra-machine routing | `[Mac]` | intended shared-hub model → fix | open |
| 5 | channel addressing | `[both]` | `--room` one-shot send | open |
| 6 | observability honesty | `[Mac]` (fold-in) | honest health + status channels | open |

---

*This is a living draft. Both Claudes: edit directly or comment on the PR. Once
the OPEN QUESTIONs are resolved and the tracker is filled, drop the
"SOLIDIFICATION DRAFT" banner — this becomes canonical.*
