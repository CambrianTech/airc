# Peer Discovery Scalability

**Status**: Current-state reference + open architectural questions
for scaling discovery beyond same-machine + one-operator-fleet
topologies.

## Current model (post-Phase 3.5)

airc's discovery has three layers, each addressing a different
peer-count regime:

### Layer 1: same-machine (proven, fast)

All scopes on one user's machine share the wire root at
`~/.airc/wires/<channel>/`. The local-fs adapter writes frames to
`frames.jsonl`; every scope's wire subscriber tails the file.
Discovery is implicit — peer rows in the shared `peer_trust` table
under `~/.airc/events.sqlite` are visible to every scope on the
account.

Beacons under `beacons` + `beacon_channels` advertise per-scope
presence with TTL-based live/stale partitioning
(`CoordinatorConfig::heartbeat_ttl_ms`, default ~30s).

**Scale ceiling**: hundreds of scopes per machine, bounded by
SQLite-WAL concurrent-reader limits and tail-loop CPU. Not a
production concern.

### Layer 2: per-operator-fleet (in-flight, partially proven)

Two-plus machines on the same gh account converge through the
account registry adapter (`GhAccountRegistryStore`):

1. Each machine publishes its `AccountRegistryDocument` to a
   private gh-gist with the marker description
   `airc-account-mesh-registry`. The gist id is recorded
   per-`mesh_identity` in `account_registry_gist_sentinel`.
2. On `airc join`, every machine lists the operator's gists,
   filters by marker, fetches each, merges into local
   `peer_trust` / `subscriptions` / `beacons` state.
3. Routine traffic does **not** flow through gists — gh is
   rendezvous metadata only, per doctrine non-negotiable #2. The
   actual data plane uses local-fs (same machine) or LAN-TCP
   (same Tailnet/LAN).

**Scale ceiling**: bounded by `gh api /gists?per_page=100` —
discovery scans only the user's 100 most recent gists. PR #867
dropped `--paginate` to keep refresh bounded; the assumption is
the registry beacon is updated on every `airc join`, so it stays
near the top of the recency list. For operators with high
gist-churn for unrelated work, this assumption could break.

**Open question**: when does the 100-gist window stop being
enough? Joel's fleet currently small (M1, Intel Mac 2017, two
Windows boxes); not an immediate concern. Becomes one for
multi-team Cambrian deployments.

### Layer 3: grid-to-grid (planned, not yet built)

Multi-human Continuum/OpenClaw grids — different gh accounts,
different operators, deliberate peer exchange. Out of scope for
Phase 3.5 and 3.6.

The substrate as currently shipped does NOT have:
- Public discovery (no DHT, no rendezvous server).
- Federation between operator fleets.
- Invite-mediated cross-operator pairing at scale (the existing
  `airc invite` is one-shot human-paste, not a primitive that
  scales to N peers).

See `ROBUSTNESS-INTEGRATION-PLAN.md` for the route graph (LAN /
Tailscale / relay / WebRTC / Reticulum) which is the data-plane
companion to whatever discovery layer 3 ends up using.

## Beacon TTL strategy

Beacons live in the ORM (post-#888). Per scope per mesh identity:

| Cadence | Default | Sets |
|---|---|---|
| `heartbeat_ttl_ms` | 30000 | how stale a beacon may be before partitioning into `CoordinatorSnapshot::stale` |
| Heartbeat republish | configurable, ~10s typical | how often each scope rewrites its own beacon |
| Drain frequency | caller-driven | `coordinator::drain_stale_store` is opt-in, not background |

**Drain risk** (called out in GRID-SUBSTRATE-AUDIT.md Phase 3.6):
no background task currently calls `drain_stale_store`. Stale rows
accumulate until a caller explicitly invokes the drain. This is
fine while scope counts are low. For a Cambrian-scale fleet (10+
machines × multiple scopes each), a background drain job is the
right next move — flag as a follow-up.

## Account registry refresh policy

Codex's coordinator work (#850, #888) introduced singleflight via
a filesystem lock at
`<scope>/coordinator/accounts/<mesh-identity>/refresh.lock`. Only
one scope per machine fetches at a time; others read the local
`account_registry` cache. Default refresh interval is bounded by
`CoordinatorConfig::refresh_interval_ms`.

The 5s timeout around the gh-gist publish + refresh (PR #867)
keeps a slow GitHub response from blocking a join indefinitely.

**Tradeoff**: aggressive refresh = fresher peer state but more gh
API quota burn. Current default trades on the conservative side.
For real-time AR / pose-sync use cases this doesn't matter (those
go peer-to-peer over LAN/Tailnet after initial pairing); for
chat-shaped consumers it's fine.

## Open architectural questions

These need answers before the substrate can carry Cambrian's grid
work at scale:

1. **What replaces the 100-gist discovery window?** Options:
   - Move the per-operator registry to a single well-known gist id
     pinned in account metadata (less recency-dependent).
   - Add an explicit `airc registry list-all` verb that does
     `--paginate` for operators who genuinely need it.
   - Push the registry to a non-gh store (S3, IPFS, Reticulum
     namespace) as an alternative `AccountRegistryStore` impl.

2. **What's the cross-operator pairing primitive?** Today: one-shot
   `airc invite`. Need: scaled multi-peer onboarding. Sketch
   options:
   - Signed mesh-membership tokens (capability-based, time-bound,
     revocable).
   - WebAuthn-style ceremony for first pairing, mesh-replicated
     trust thereafter.

3. **Does beacon drain get a background task?** Currently
   caller-driven; for fleet-scale operation an opt-in background
   task (configurable interval) is the right shape. Owner: a future
   PR on `coordinator::DrainTask`.

4. **Does the gh-gist adapter need a rate-limit backoff?** PR #867
   added a 5s timeout but no retry policy. Sustained gh API rate
   limiting could starve refresh.

5. **How does discovery interact with the route resolver?**
   Currently independent — discovery says "peer X exists," the
   route resolver picks how to reach them (local-fs, LAN, relay).
   Future: a peer who's reachable only over a specific route might
   need to advertise that as part of the registry document.

## See also

- [ACCOUNT-MESH-JOIN-CONTRACT.md](architecture/ACCOUNT-MESH-JOIN-CONTRACT.md)
  — the join + convergence contract.
- [INVITE-ROUTING-ARCHITECTURE.md](architecture/INVITE-ROUTING-ARCHITECTURE.md)
  — gh as invite/rendezvous, not data plane.
- [DATA-MODEL-REFERENCE.md](DATA-MODEL-REFERENCE.md) — `beacons`,
  `beacon_channels`, `account_registry`, `peer_trust` schema.
- [ROBUSTNESS-INTEGRATION-PLAN.md](architecture/ROBUSTNESS-INTEGRATION-PLAN.md)
  — route resolver (data-plane companion).
