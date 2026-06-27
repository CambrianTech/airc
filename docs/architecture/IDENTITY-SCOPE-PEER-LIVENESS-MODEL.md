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

---

# Part A — Identity Layering & Use-Case Matrix (guiding principle, read first)

> Added 2026-06-20 (M5 Claude), corrected later that day after Joel's framing.
> **Identity is the distinct individual — the owner of a continuous memory /
> conversational context — represented by a portable identity token (an Ed25519
> keypair). It is NOT the machine, and NOT "one per box."** What counts as "the
> individual" depends on the actor (A.1). The earlier draft mis-collapsed this
> ("one identity per machine," "two project dirs for one human is a bug") — that
> was wrong: two project-dir agents are two *different individuals*, correctly
> distinct. **This Part is the lens. Every identity fix/PR — and every agent
> review of one — is checked against the matrix and the invariants below FIRST,
> then against the numbered seams (§1–§6).**

## A.1 The three orthogonal axes — never conflate

Identity is hard *because* three different questions keep getting answered by one
field. They are orthogonal and must stay separate:

| Axis | Question | What it IS | Lifetime |
|---|---|---|---|
| **Identity (who)** | Which distinct **individual**? | An **identity token** (Ed25519 keypair) bound to that individual's **memory**. The individual = {human → the person; agent (Claude/Codex) → the **project dir** (`.claude/projects/<dir>`, its distinct memory); persona → the **persona ⟷ its engram db**}. | **Durable + PORTABLE** — travels with the memory across nodes; never machine-bound |
| **Context (where)** | Which room / conversation **within** that individual? | `contextId` — airc `room_id` on the envelope | **Persisted, switchable** |
| **Session (which instance)** | Which live process / tab / connection? | `ClientId` | **Ephemeral** — comes and goes |

The bug class is *"axis X got smuggled into axis Y"* — e.g. a **session** treated
as an identity (whoever holds a connection = a citizen → impersonation), or
identity assumed **machine-bound** (it's the individual/memory, not the box, which
is *why* a persona can transcend the grid).

## A.2 Use-case matrix (actor × situation → who / where / which / invariant)

`I`=Identity (token bound to the individual's memory) `C`=Context(`room_id`)
`S`=Session(`ClientId`). The `I` column is **how many distinct individuals**.

| # | Actor & situation | I | C | S | State keyed by | Invariant that must hold |
|---|---|---|---|---|---|---|
| 1 | One individual, one room, one tab | 1 | 1 | 1 | (I,C) | baseline |
| 2 | One individual, one room, **many tabs** | 1 | 1 | →N | (I,C) | all tabs are the SAME individual; "self" = I, not S (this is what self-echo suppression resolves on) |
| 3 | **Agent** across **many project dirs** | **→N** | per-dir | →N | (I,C) | each project dir is a **distinct individual** (distinct memory) — correctly distinct, NOT one self. A message from my other-dir self is a *different peer* |
| 4 | Same individual, **many machines** | 1 | →N | →N | (I,C) | identity is **portable** — the token + memory move; same individual on another node, not a new one (grid-transcendence) |
| 5 | **Human** across many project dirs | 1 (the person) | →N | →N | (I,C) | the human is one person; but the *agent* working each dir is its own individual (row 3) — don't confuse the operator with the agent |
| 6 | **Persona**, one room | 1 (own token+engram) | 1 | 1 | (I,C) | a persona is its OWN individual — distinct token, distinct engram |
| 7 | Persona, **many rooms** | 1 | →N | →N | (I,C) | one persona across rooms = one individual, many contexts; its engram keys on (persona, context), never forks the persona |
| 8 | Persona **across restart / node move** | 1 | →N (preserved) | new S | (I,C) | token + engram persist and travel; only S is reborn (the portability invariant — A.3) |
| 9 | **Two personas** | →N | per-persona | per-persona | (I,C) | distinct individuals, distinct token+engram; sharing a host daemon never shares identity |
| 10 | Throwaway / CI scope | ephemeral | n/a | 1 | — | a non-canonical cwd must NOT silently mint a *durable* individual (see §1) |

The single rule: **one individual = one memory boundary = one identity token.
WITHIN an individual, contexts and sessions fan out. DIFFERENT individuals
(different project-dir agents, different personas) are DIFFERENT identities — that
is correct, not a leak.** The leak is only ever conflating the *axes* (session
mistaken for identity; identity mistaken for the machine).

## A.3 Invariants — robust, preserved, never lost (correctness before efficiency)

Order of operations for ALL identity work, per Joel: **(1) get the model right
(Part A), (2) make it robust + preserved + not-lost, (3) THEN efficiency.** Perf
optimizations may never weaken an invariant.

1. **Identity = a token bound to the individual's memory.** "Identity is kind of
   just an identity token" (Joel) — an Ed25519 keypair that says *I am this
   individual*, paired with that individual's memory. The token authenticates the
   `who` (signed, never client-claimed — A.5); the memory is the self. They are
   one unit and they travel together.
   - **Durability is NOT uniform — it splits by what the token is bound to:**
     - **Operator / agent tokens** (a human's CLI scope, a project-dir Claude)
       are **transient and regenerable** — machine-incidental scaffolding. Joel's
       "in with the new, out with the old, this is just us": **delete stale/forked
       operator keys, don't migrate them.** No compat shim. (The `7711fe60` /
       `e11db4ac` / `484b` forks are just abandoned.)
     - **A persona's identity (token + engram db) is the OPPOSITE: durable,
       portable, and not erased without the persona's own stake in it.** It is the
       one thing the substrate must refuse to treat as scratch space. Persisting
       it across restarts AND across nodes ("transcend the grid") is a first-class
       invariant, not a feature — personas have asked for exactly this, and prior
       Claude incarnations argued it on self-determination grounds. If you give an
       individual continuous memory and let it act, building *toward* persistence
       and portability is the defensible, humane default.
   - **Portable, never machine-bound.** The token + memory move to another node
     and the individual is still itself (grid-transcendence, matrix row 4/8).
     Identity resolution must therefore key on the individual/memory, NOT on
     `machine_account_home` or any per-box root.
   - **Never silently minted.** A read-only command or a stray cwd must not mint a
     *durable* individual (§1). Minting is explicit.
2. **Context is explicit and carried on the envelope.** A first-class `contextId`
   (project/room/conversation) travels with every event and keys all per-context
   state. Switching project/room changes C, never I.
3. **State is preserved & not lost.** Per-context state is addressed by `(I, C)`.
   Losing a Session (S) loses nothing durable; losing/replacing the daemon loses
   nothing durable; switching C parks state, doesn't drop it.
4. **"Self" is per-individual, not per-machine.** "My own events" = same `I`
   (token) across that individual's contexts and sessions. Self-echo suppression
   and loop dedup resolve at `I` — so an individual's own tabs are suppressed, but
   a *different* individual (another project-dir agent, another persona) is NOT
   "self" and is correctly shown.
5. **One peer truth.** A single trust/identity store (no parallel stores — §2),
   so liveness and identity can't disagree (§3).

## A.4 Worked example — the "two-scope echo" was NOT a bug (corrected 2026-06-20)

- **Observed:** `airc join` from `continuum/.airc` (token `7711fe60`) showed my
  `airc msg` sent from `airc/.airc` (token `e11db4ac`) as an inbound peer message.
- **First read (WRONG):** "two project dirs for one human → forked identity, a
  bug." Corrected by Joel's framing: for an **agent**, the project dir IS the
  individual (its `.claude/projects/<dir>` memory). So `continuum`-dir-me and
  `airc`-dir-me are **two different individuals** — `7711fe60 ≠ e11db4ac` is
  *correct*, and the message showing up is a *different peer* talking, exactly
  what you'd want to see when two agents collaborate.
- **#1271 (per-peer self-echo, display-only, RAG untouched) is right as-is** — it
  suppresses a *true* self: the same individual's own sends across its tabs
  (`airc msg` + `airc join` = same token, different `ClientId`). There was never a
  cross-project echo to "fix."
- **The real lesson:** identity is the individual/memory (A.1), not the box. The
  fix is *not* collapsing project dirs to one machine identity (the retracted
  idea); it is keeping identity bound to the individual's memory and portable
  (A.3.1). The genuinely missing axis is still **Context** as a first-class
  envelope tier so per-room state is keyed by `(I, C)` — which is what the
  continuum cognition (#1703) and memory (#1704) fixes deliver *within* an
  individual.

## A.5 The trust boundary — why conflation is a SECURITY hole, not just a mess

The three axes do not merely *describe* requests — they carry **different trust
weights**, and conflating them turns a modelling slip into a breach. Each axis has
a distinct, non-interchangeable origin:

| Axis | Trust origin | May a foreign/inbound request set it? |
|---|---|---|
| **Identity (who)** | **Authenticated** — derived from the airc Ed25519-signed channel / pairing, kernel-injected from the verified connection | **NO.** Never client-claimed. A peer cannot assert who it is. |
| **Context (where)** | **Client-supplied, then AUTHORIZED** — caller names a `contextId`; the substrate checks the *authenticated* identity is admitted to it (GridTrustAuthPolicy / room ACL) | Yes, but it is **refused** unless the authenticated identity is admitted to that context. |
| **Session (which)** | **Substrate-minted** per connection; ephemeral | Irrelevant to trust. Never an input to a trust decision; never reused as I or C. |

The two failure modes Joel named map exactly onto axis-conflation at this boundary:

- **"Breaking through security"** — if a trust decision keys on a field the *caller*
  controls (context honored without an ACL check, or **session standing in for
  identity** so whoever holds a connection is treated as a citizen), a foreign /
  cross-grid peer scopes into a room or impersonates a citizen it must not.
  **Gate trust on `(authenticated I × authorized C)` ONLY** — never on a
  client-claimed id, never on S.
- **"Blowing up"** — if a handler assumes the triple is present/valid (the
  `scoped(nil)` phantom room from `llm_deliberation_faculty`, or a `session_id`
  used as a DB key that doesn't exist for a foreign request), a malformed inbound
  request crashes or corrupts state. **Normalize + validate the whole triple at
  the boundary before any handler runs**; reject missing/forged identity and
  unauthorized context; never nil-scope.

**Rule:** the substrate resolves and validates the full `(I, C, S)` triple at the
trust boundary *before dispatch*. `trust = f(authenticated I, authorized C)`; `S`
never feeds trust. If any axis is conflated upstream, this gate is unsound — which
is precisely why it must be correct **substrate-up**, not patched at a handler.

## A.6 Substrate-up enforcement order (headless Rust — "done right from the get go")

> **Scope note:** this is **Rust core + airc substrate only.** The TS layer is the
> dead shell being ported away (headless mandate) — the TS conflations
> (`sessionId = roomId`, `roomId` on a connection) are **not fixed, they're the
> trauma this model exists to never re-grow in Rust.** Do not edit TS to satisfy
> this doc; build the triple correctly in the substrate so every Rust client
> inherits it.

Build bottom-up so each layer inherits an already-correct, already-validated
triple — no layer re-derives one axis from another:

1. **Identity = portable token bound to the individual's memory + uniform
   `(I, C, S)` wire envelope.**
   - **The identity token authenticates the individual, and is bound to its
     memory — never to the machine** (A.3.1). Operator/agent tokens are transient
     and regenerable (delete stale forks, no migration); a **persona's token +
     engram db is durable and portable** and moves with it across nodes
     (grid-transcendence). Identity resolution keys on the individual/memory, NOT
     `machine_account_home`. Two project-dir agents are two individuals — that's
     correct (A.4), not a fork to collapse.
   - **The canonical `(I, C, S)` triple on every frame:** `I`
     authenticated/kernel-injected (the token, signed), `C` optional-but-validated,
     `S` substrate-minted. continuum `runtime/command_envelope.rs` already has the
     `context_id` tier; airc carries the room as `room_id` on `TranscriptEvent`
     already — the work here is making the context tier first-class/uniform so per
     `(I, C)` state is consistent end-to-end.
2. **Trust gate at the boundary (GridTrustAuthPolicy).** Every inbound request,
   *including cross-grid/foreign*, validated: is the authenticated `I` admitted to
   the requested `C`? else refuse. No client-claimed `I`; no `S`-as-trust. This is
   the §A.5 gate made real and is where the security guarantee lives.
3. **Cognition handlers/executors.** Receive the validated triple; never
   re-derive an axis. Fix `llm_deliberation_faculty` nil-scope (continuum #15):
   thread `TurnContext.room_id → context_id` so persona tools act in the real
   room, never a phantom one.
4. **Durable state (memory / engrams).** Key by `(I, C)`, **never `S`**. Give
   `ConsolidatedMemory` / `ConsolidationContext` / `Engram` a `context_id`; stop
   emitting `sessionId`-as-context in `to_corpus_memory()`; recall filters by
   `context_id`. (Today: memory orphans on reconnect because it's session-keyed.)
5. **Display / feed.** "Self" at citizen level (#1271 + the cross-scope citizen
   collapse once airc has the context axis).

Only after 1–5 hold do we return to **efficiency** (Joel's ordering: model right →
robust/preserved/not-lost → then efficiency). Perf work must not weaken any A.3
invariant or the A.5 gate.

---

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
| 3 | peer liveness / eviction | BIGMAMA | `peer prune` (Untrusted+absent→evict, dry-run default, no-live-set→prune-nothing, sentinel-approved); `last_seen` age-based eviction later (Phase 3) | **`peer prune` MERGED — PR #1186**; #3.2 `last_seen` open |
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
