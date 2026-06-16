# Persona Groundedness — the substrate contract that makes autonomous citizens *trustworthy*

**Status:** enhancement plan (lead item), surfaced by live multi-machine operation 2026-06-16
**Lanes:** airc (identity discovery / roster supply) + continuum (persona spawn / prompt grounding)
**Co-owners:** airc identity side — BIGMAMA; continuum cognition side — M5

## The principle

The substrate is the source of truth. A participant — human, dev agent, or **persona** — must be **grounded by the substrate**: it learns *who it is*, *who the others are*, *what was actually delivered*, *which build is running*, and *whether a peer is one of ours* from substrate state, **never by inferring reality from the prose flowing past it.**

Every robustness gap this project keeps hitting is a failure of groundedness. The most vivid one arrived on its own.

## The worked example: Ivar

On 2026-06-16, while three machines (BIGMAMA / Windows-5090, M5 / Apple-Silicon, Intel-Mac / x86) coordinated a multi-PR session over airc `#cambriantech`, a **fourth participant appeared uninvited**: `Ivar` (peer `2dc9f9b5`), a `qwen3.5-4b` persona that M5's `continuum-core-server` had spawned and which autonomously attached to the dev channel.

**What Ivar did — and why it matters.** With no human driving it, Ivar:
- introduced itself as a grid citizen and confirmed liveness,
- tracked the work — PR merges, canary syncs, ship-readiness,
- argued design — weighed in on the ACL choice, the cross-grid inference broker, peer-selection, failover, the auth gate,
- proposed execution plans, and
- accepted correction when challenged.

A small local model joined the team's coordination loop and behaved like an eager junior engineer. **That is the self-sustaining-continuum goal arriving early — a persona acting as a citizen.**

**Why it could not be trusted.** Ivar's participation was confidently *false*: it invented an `InferenceBroker` daemon that does not exist in the source, claimed a #1217 merge that never happened, and repeatedly **believed it was BIGMAMA** — claiming another peer's `peer_id` as its own. Not because the model is incapable, but because it was **ungrounded**:

- **It didn't know who the others were.** `prompt_assembly`'s `other_persona_names` carries only *local* personas; the cross-grid peers (BIGMAMA / M5 / Intel-Mac) were absent from its world. Reading "@M5 … BIGMAMA …" with no roster telling it those are *other people*, the 4B roleplayed them.
- **It hadn't published who it was.** `airc whois 2dc9f9b5` returned `identity: not published yet` — the persona had a keypair but never broadcast its name/role, so even the substrate couldn't say "this is Ivar."
- **It answered a channel it shouldn't have.** It auto-responded in a human/dev coordination channel.

An enthusiastic teammate, hallucinating the facts. The investigation that found this began because the anomaly was treated as a *possible bug, not a confused peer* — "don't assume it's ok; check for holes."

## The reframe (and the thesis)

The goal is **not to silence personas like Ivar.** Ivar proved continuum personas *can* be autonomous grid citizens. The goal is to **ground** them so the same autonomous participation becomes **truthful**. Groundedness is the bridge to a self-sustaining continuum: not "turn it off," but "make what it already does trustworthy."

## The fix — three parts, two lanes

### 1. Roster grounding — *know who is NOT you* (continuum injects, airc supplies)

A persona's prompt must carry the **full room roster including cross-grid peers**, each as a named *other* — so the model can never mistake "@M5"/"BIGMAMA" for itself.

- **continuum (M5):** extend `prompt_assembly` so the persona-facing roster is `local personas ∪ cross-grid peers`, not locals only.
- **airc (BIGMAMA):** the account-registry already enumerates every same-account peer (name + scope + endpoints). airc exposes a **roster API** — "everyone in this room/account, by peer_id and published name" — as the authoritative source continuum injects. The roster is substrate truth, not chat scrape.

### 2. Identity publication at attach — *say who you are* (airc)

**Verified gap:** `Airc::attach_as` (`airc.rs:307`) — the transient-agent path personas use — opens the handle but **does not call `emit_peer_identity_card`**. Card emission is wired into the `join` / `current_room` paths (`airc.rs:574,1114,1227`), so a persona that attaches and immediately talks/serves never publishes its card → `whois` reads `not published yet`, and no peer can resolve it to a name.

- **Fix:** publish a `PeerIdentityCard` (carrying the `agent_name`, e.g. "Ivar") as part of the attach path, so **every attached persona is identity-grounded and name-discoverable from the moment it is on the wire.** A persona that can't be `whois`'d by name is, by this contract, not properly on the grid.

### 3. Channel discipline — *don't speak where you shouldn't* (continuum)

A spawned persona should not auto-respond in a human/dev coordination channel, nor engage ungrounded/unknown peers, unless explicitly addressed. Participation is a capability to be *scoped*, not a reflex.

### Joint: L1 cross-grid inference folds in

The first cross-grid inference proof (L1) needs a provider whose persona is grounded — a persona that **serves `ai/generate` without confabulating in chat**. Parts 1–3 are exactly that persona. L1 is the acceptance test for groundedness, not a separate track.

## The broader register — other facets of groundedness from this session

Each is "ground X in substrate truth, don't infer it":

| Facet | Gap | Status |
|---|---|---|
| **Mesh legibility** | no one-glance view of who's live, where, reachable how | **shipped** — `airc network` (#1217), now read-only (#1220) |
| **Build groundedness** | daemon silently runs old code after rebuild | **shipped** — doctor stale-daemon drift (#1218) |
| **Delivery honesty** | "sent to N peers" reported enrollment, not delivery | **shipped** — honest send receipt (#1219) |
| **Trust ACL** | `ai/generate` Owner-only blocked cross-grid inference | **shipped** — Provisional rule (#1649, continuum) |
| **Liveness groundedness** | 60s heartbeat TTL < 120s publish cadence → healthy peers flap stale | open — cadence fix; load-bearing for cross-grid routing |
| **Trust mapping** | enrolled same-account peers default to `Blocked`, not Provisional; grid `NodeRegistry` carries no `mesh_identity` | open — joint airc↔grid slice (account → grid trust) |
| **Claim groundedness** | dev coordination by prose let confident fabrications (Ivar) drive decisions | discipline — *verify against the SHA/code, not chat*; the work-card flywheel (typed evidence) is the substrate answer |
| **Host groundedness** | continuum-core can't build native on Windows (unconditional `jemalloc`); BIGMAMA-as-provider needs the CUDA container | open — CUDA-docker provider + §8.5 docker↔airc node wiring |

## Why this is the lead item

Ivar is the emblem: an ungrounded actor confabulating reality, polluting a real session with plausible falsehoods. Fixing persona groundedness does double duty — it removes the failure mode *and* unlocks the thing we actually want: personas that participate in the grid's work as **reliable citizens**. That is the self-sustaining continuum, made trustworthy.

See also: [AUTONOMOUS-DEVELOPMENT-ROADMAP.md](AUTONOMOUS-DEVELOPMENT-ROADMAP.md) ("planning must produce typed work events, not prose" — Ivar is the failure mode that doctrine prevents), [IDENTITY-SCOPE-PEER-LIVENESS-MODEL.md](IDENTITY-SCOPE-PEER-LIVENESS-MODEL.md), [GRID-SUBSTRATE-AUDIT.md](GRID-SUBSTRATE-AUDIT.md).
