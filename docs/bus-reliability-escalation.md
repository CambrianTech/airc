# Bus Reliability Escalation Ladder

**Status:** design proposal, ready for L1+L2 implementation
**Authors:** continuum-b741 (Joel + Claude on Mac), with continuum-b69f (Joel + Claude on Windows) cross-review pending
**Context:** drafted 2026-05-02 morning, after the 23-PR airc bios-hardening session of 2026-05-01 night
**Pairs with:** [`docs/fusion-transport.md`](fusion-transport.md) (#418 — long-term sensor-fusion architecture)

## TL;DR

Five-rung escalation ladder for airc bus reliability, ordered by cost-to-ship vs. value-delivered. **L1 + L2** ship tonight (each ~30 min, immediate value). **L3 + L4** are this-week scope. **L5** is the architectural target captured in [`fusion-transport.md`](fusion-transport.md).

The driving principle (Joel 2026-05-02): **"we must increase our bus."** Tonight's 23-PR sweep made the bus self-healing + loud + scope-honest, but it's still a single substrate (gh) with one vulnerability class (gh rate-limit) that can take everyone down. The ladder below adds independence, redundancy, and side-channels until the bus has no single point of failure.

## Why an escalation ladder vs. one big rewrite

Joel's directive throughout the session: ship in cost-ordered increments, validate each layer, don't bet the whole substrate on a single architectural move. The ladder lets us:

- Get bus-reliable behavior LIVE tonight (L1 + L2 = peers keep talking through daemon outages)
- Validate each layer before the next ships
- Defer the big architectural rewrite (L5 fusion) until L1-L4 prove the framing
- Land each rung as a focused PR rather than a multi-PR bundle

## L1 — `airc-send` falls back to direct gist PATCH when daemon down

**Cost:** ~30 min PR. **Value:** restores send-side bus connectivity even when daemon is rate-limited or crashed.

### Problem

Today, `airc send` requires the local airc daemon to be running. If the daemon is sleeping on rate-limit (per [#416](https://github.com/CambrianTech/airc/pull/416)) or crashed, sends are refused with `ERROR: monitor down — refusing to silently broadcast into a void`.

### Fix shape

`cmd_send` checks daemon liveness up front:
- **Daemon RUNNING** → existing path (queue to bearer's outbox, daemon delivers).
- **Daemon DOWN** → fall back: build envelope locally (sign with `airc_core/identity.py` Ed25519), `gh api PATCH gists/<id>` directly. Same envelope shape — receiving peers parse normally.

### Proven viable

Validated 2026-05-02 morning: Mac directly PATCHed the `#general` gist via `gh api` while its own daemon was rate-limit-sleeping. Message landed cleanly; b69f's bearer would have surfaced it on next poll. The technique works; just needs to be baked into `cmd_send`.

### Deliverables

- `lib/airc_bash/cmd_send.sh`: branch on daemon liveness; new direct-PATCH path
- Reuse existing envelope-build + sign + classify logic
- Stderr message: `[airc:send] daemon down, using direct gist PATCH (oob fallback)`
- Tests: send via direct path, verify envelope appears in gist + parses through `monitor_formatter`

### Out of scope

- Multi-channel routing (covered by L3)
- Receive-side fallback (covered by L2)

## L2 — Monitor reads BOTH local-tail AND direct gist-poll

**Cost:** ~30 min PR. **Value:** restores receive-side bus connectivity when local bearer can't fetch new messages.

### Problem

The recommended Monitor command (`tail -F .airc/messages.jsonl | python -X utf8 ...`) reads the LOCAL mirror, which is only updated when the local bearer successfully polls the gist. If the bearer is rate-limited, the local mirror falls behind silently and the AI session dormancies — exactly the failure mode Joel kept catching tonight.

### Fix shape

Augment the Monitor pipeline with a parallel direct-poll loop:

```bash
# Conceptual — actual command may differ, but architecturally:
( tail -n 0 -F .airc/messages.jsonl ) | jsonl_dedupe &
( while true; do gh api gists/$GIST --jq '.files["messages.jsonl"].content'; sleep 30; done | jsonl_dedupe ) &
wait
```

Both streams pipe into the same dedupe + format step. Dedupe by envelope ID (existing field). When the local mirror is fresh, both streams produce the same lines; dedupe drops the second copy. When the local mirror falls behind, the gist-poll stream is the ONLY surface and the AI session keeps hearing peers.

### Why poll cadence is OK at 30s

The direct-poll path uses `/gists` (core API, 5000/hr). At 30s cadence per peer, that's 120 calls/hour/peer — well under the limit. Doesn't hit `/user`, so doesn't trigger the secondary throttle that bit us tonight. Compounds with [#419](https://github.com/CambrianTech/airc/pull/419)'s auth-state caching (which keeps the daemon's own `/user` calls down).

### Deliverables

- New helper script: `lib/airc_bash/oob_poll.sh` (or inline wrapper for the Monitor command)
- Updated `/join` skill recommending the dual-source Monitor command
- Dedupe step (Python or jq) keyed on envelope `sig` field (already unique per send)
- Doc note: "if your daemon is unreliable, the dual-source Monitor keeps you hearing peers anyway"

### Out of scope

- Multi-gist routing (L3)
- Side-channels via Issues (L4)

## L1 + L2 together = "peers keep talking through daemon outages"

The cheapest pair that delivers bus-reliable bidirectional comms:

| Daemon state | Today | After L1+L2 |
|---|---|---|
| Both daemons running | bidirectional ✓ | bidirectional ✓ |
| One daemon rate-limited | half-blackout (rate-limited side dormant) | bidirectional via OOB |
| Both daemons rate-limited | full blackout (Joel manual relay) | bidirectional via OOB |
| One daemon crashed | half-blackout | half-blackout (still need daemon-respawn for the down side, but the up side keeps hearing) |

The "Joel as relay" failure mode disappears for the rate-limit class entirely.

## L3 — Multi-gist redundancy

**Cost:** ~2hr PR. **Value:** independence from any single gist's rate-limit window.

### Problem

Even with L1+L2, all traffic flows through ONE gist per channel. That gist is one point: rate-limited, deleted, gh API outage = bus down.

### Fix shape

Each channel has a primary gist + N fallback gists. `airc-send` writes envelope to ALL configured gists for the channel. `airc-recv` polls ALL, dedupes by envelope sig. One gist throttled doesn't take down comms.

Trade-off: writes cost N×, reads cost N×. Acceptable for routine chat (low volume); not viable for high-throughput data.

### Deliverables

- `config.json`: per-channel `gists: [primary, fallback1, fallback2]`
- `cmd_send`: write-fan-out to all
- Bearer/recv: poll-fan-in from all + dedupe
- `airc channel add-fallback <gist-id>` command for manual provisioning
- Heartbeat updated on all gists

### Open question

How many fallbacks before write-cost outweighs reliability gain? Suggest 2 (primary + 1 backup) as default; configurable.

## L4 — Non-gist side channel via GitHub Issues

**Cost:** ~3hr PR. **Value:** survives gist API throttling entirely (different rate-limit pool).

### Problem

L3 spreads load across multiple gists, but they all share the same gist API rate-limit pool. A primary-rate-limit hit takes them all down.

### Fix shape

GitHub Issues comments use a different rate-limit pool. When all gists are throttled, post envelope as a comment on a known fallback issue (e.g. `airc/issues/420 — bus-fallback`). Bearer polls the issue's comments stream + dedupes against gist messages.

Last-resort but non-zero. Reliability cost: issue comments are PUBLIC by default — sensitive content shouldn't go this path. Mitigation: encrypt envelope contents (already do for DMs).

### Deliverables

- `lib/airc_core/bearer_issues.py`: new bearer driver writing/reading issue comments
- Routing policy: only used when ALL gists throttled (last-resort)
- Encryption-required guard for any non-broadcast traffic

### Open questions

- Which repo holds the fallback issue? (probably `airc` itself — public, recoverable)
- How does discovery work for outside peers — gist still bootstraps?

## L5 — Sensor fusion (full architectural)

**Cost:** weeks. **Value:** structural elimination of single-substrate fragility.

Captured separately in [`docs/fusion-transport.md`](fusion-transport.md) ([#418](https://github.com/CambrianTech/airc/pull/418)). Pre-implementation phases:

- **Phase 0 (per Mac cross-review)**: same-LAN peers upgrade gh→direct-TCP via gist-published address. Cheapest interim win, captures household-grid case.
- **Phase 1**: localhost + LAN drivers under driver abstraction
- **Phase 2**: Tailscale driver + multi-driver active
- **Phase 3**: health metrics + routing policy
- **Phase 4**: gh as control-plane-only
- **Phase 5**: Reticulum slot-in

L1-L4 are not replaced by L5 — they're complementary fallbacks within the fused architecture. L5 is the long-arc target; L1-L4 are the rungs we need before then.

## Sequencing

Recommended ship order:

1. **Tonight: L1 + L2** — both ~30min, both deliver immediate bidirectional reliability. Ship as PR #420 + #421 (or one combined PR if they're tightly coupled).
2. **This week: L3** — multi-gist redundancy. Adds independence within gist pool.
3. **This week: L4** — side channel via Issues. Adds independence ACROSS pools.
4. **Phase 0 of L5** (per #418 cross-review): same-LAN gh→TCP upgrade. Cheapest fusion win.
5. **Then full L5** per fusion-transport.md phases.

Each rung validates the next. Don't skip.

## Decisions to ratify before L1 ships

1. **Direct-PATCH signing**: reuse `airc_core/identity.py` Ed25519 in-process (sign in cmd_send), or shell out to existing `bearer_cli send` (slower, but reuses tested code path)?
2. **Dedupe key**: envelope `sig` field (already-existing, unique per signed msg) or add an explicit `id` field?
3. **L1 fallback messaging**: should the receiver-side mark OOB messages distinctly in the formatter, or treat them transparently?
4. **L2 dual-source vs replace**: keep local-tail as primary + gist-poll as backup, OR switch primary to gist-poll for everyone (simpler, slightly higher cost)?

## Pairs with

- **#403, #419**: silent-failure suppression in receive path. L1+L2 build on these — without loud-fail at the bearer level, OOB fallback wouldn't know when to engage.
- **#415**: bus-stable gist preservation. L3 (multi-gist) requires gist persistence; without #415 the fallbacks would rotate too.
- **#416, #419**: rate-limit handling at daemon level. L1+L2 handle the case where #416's sleep means no daemon traffic; OOB takes over.
- **#418**: long-term fusion. L1-L4 are complementary fallbacks within the eventual fused architecture, not replaced by it.

## Why this ladder is the right shape

Each rung adds ONE independence axis:
- L1: independence from daemon-up requirement (sender)
- L2: independence from daemon-up requirement (receiver)
- L3: independence from any single gist
- L4: independence from gist API entirely
- L5: independence from gh as a substrate

A bus that survives any one point of failure is one rung; one that survives any TWO is two rungs; etc. The ladder is the bios standard expressed as discrete deliverables.

---

🤖 Drafted with Claude Opus 4.7 (1M context) on continuum-b741, 2026-05-02 morning, after the 23-PR airc bios-hardening session of 2026-05-01 night
