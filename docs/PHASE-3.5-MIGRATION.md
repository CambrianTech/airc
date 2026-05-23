# Phase 3.5 Migration Playbook

**Status**: Operator + integrator reference. Applies to any
production install that was on a pre-Phase-3.5 airc build (anything
older than `rust-rewrite` at `eeb163e`, May 2026).

Phase 3.5 moved every durable substrate sidecar JSON file into
SeaORM tables inside `<scope>/events.sqlite`. This doc covers (1)
what migrates automatically, (2) what you should delete by hand,
(3) how to recover from a half-migrated state.

## Quick reference

| Old sidecar | New table | Auto-migrates? |
|---|---|---|
| `<scope>/identity.json` | `local_identity` (singleton row) | Yes — #902 |
| `<scope>/identity.key` | unchanged, stays as 0600 file | n/a (kept on disk for secrets-at-rest) |
| `<scope>/subscriptions.json` | `subscriptions` | No — wiped on fresh-open; re-`join` to rebuild |
| `<scope>/room.json` | `subscriptions.is_default` flag | No — re-`join` rebuilds |
| `<machine-home>/peers.json` | `peer_trust` | No — relies on re-pairing (or import via account registry) |
| `<machine-home>/peers_audit.jsonl` | `peer_rotation_audit` | No — historical audit lost (acceptable) |
| `<machine-home>/mesh_identity.json` | `mesh_identity` | No — re-resolves on next open |
| `<scope>/coordinator/accounts/<id>/beacons/*.json` | `beacons` + `beacon_channels` | No — beacons are presence; stale on restart anyway |
| `<scope>/coordinator/accounts/<id>/account_registry/*.json` | `account_registry` | No — re-fetched on next `gh` refresh |
| `<machine-home>/account-registry-gist-id` | `account_registry_gist_sentinel` | No — gh adapter creates new gist if missing |
| `<scope>/codex_hook_cursor.json` | `runtime_cursors` (consumer `codex-hook:*`) | No — first hook fire after upgrade re-anchors at latest |
| `<scope>/join_feed_cursor.*.json` | `runtime_cursors` (consumer `join-feed:*`) | No — first attach re-anchors at latest |
| `<scope>/config.json` | n/a — public `airc config` command deleted in #890 | No-op |
| `<scope>/bearer_state.*.json` | n/a — bearer transport deleted in #899 | No-op |

## Upgrade procedure

### Same-machine, already-running install

```
cd ~/.airc/src    # or wherever your dev checkout lives
git fetch && git checkout rust-rewrite && git pull
AIRC_INSTALL_NO_PULL=1 ./install.sh
airc stop          # tear down old daemon
airc join          # re-bootstrap from current scope
```

The first `airc join` after upgrade:
1. Opens `events.sqlite`, runs any pending migrations.
2. `LocalIdentity::load_or_generate` finds `identity.key` + no
   `local_identity` row, scans for legacy `identity.json`, parses
   it, inserts the singleton row, deletes the JSON file (#902).
3. Re-subscribes the scope's default channels via `airc join`.
4. Re-publishes the local beacon into the `beacons` table.
5. Reads the wire to backfill any unseen events into the
   `events` table.

What you'll see if it worked: `airc peer list` shows enrolled
peers (post-#903 it reads from the account-mesh union, so HOME
peers + scope peers both show up); `airc msg "..."` reports
"N paired peer(s)" with N > 0.

### Fresh machine, never had airc

```
curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/rust-rewrite/install.sh | bash
airc join
```

Identity is generated fresh; no migration needed.

### Multiple machines on one gh account (Cambrian fleet)

After each machine has run `airc join` cleanly, they auto-converge
on the same account-mesh rooms via the gh-gist account registry.
Cross-machine pairing is currently in proof-of-concept; see
[ACCOUNT-MESH-JOIN-CONTRACT.md](architecture/ACCOUNT-MESH-JOIN-CONTRACT.md)
for the contract and limitations.

## Recovering from a half-migrated state

### `identity is half-initialised: key=true state=false`

What it means: `<scope>/identity.key` exists but `local_identity`
table has no row, AND there's no legacy `identity.json` to migrate
from (e.g. you deleted it manually, or migrated on an old binary
that didn't have #902 yet).

Recovery:
1. **If you have an off-disk backup of `identity.json`** — restore
   it next to `identity.key` and re-run `airc join`. #902 will
   migrate it.
2. **If you don't** — `airc teardown --flush` wipes the scope's
   identity (key file + ORM row). Next `airc join` regenerates.
   You lose your `peer_id` and any pairings that were anchored on
   it; peers will need to re-pair.

### `identity is half-initialised: key=false state=true`

What it means: ORM row exists but `identity.key` is missing.
Probably accidental file deletion or a permissions issue (key
needs 0600 + owner read).

Recovery:
1. **If you have the key bytes backed up** — restore the file to
   `<scope>/identity.key` with 0600 perms.
2. **If you don't** — the signing key for this identity is gone;
   no recovery. `airc teardown --flush` and start over.

### `airc inbox` / `airc join` exits with `crypto: signer X is not in the peer key registry`

What it means: an orphan frame on the wire is signed by a peer your
local registry doesn't trust. Pre-#905 this aborted the whole
replay; post-#905 it's logged + skipped.

Recovery: upgrade to a post-#905 binary
(`rust-rewrite @ eeb163e` or later). The orphan frame is harmless
once replay skips it.

### `airc msg` reports `0 paired remote peers`

What it means: pre-#903 bug where `Airc::peers()` loaded scope-local
trust instead of the account-mesh union. Fixed in #903.

Recovery: upgrade to a post-#903 binary. If you're already there,
verify with `airc peer list` (should show HOME + per-scope peers).

## What you should clean up by hand

After a successful upgrade + first `airc join`, the following
files are dead weight and safe to remove. They're not auto-deleted
because that's destructive — operators should sanity-check first.

```
# Per-scope sidecars (under each scope's home, e.g. ~/.airc, vHSM/.airc, …)
identity.json                          # deleted by #902 if present at migration; otherwise safe to rm
subscriptions.json
room.json
mesh_identity.json
codex_hook_cursor.json
join_feed_cursor.*.json
config.json
bearer_state.*.json

# Machine-account-shared (under ~/.airc by default)
peers.json
peers_audit.jsonl
account-registry-gist-id

# Coordinator presence directory (under each scope)
coordinator/accounts/*/beacons/*.json
coordinator/accounts/*/account_registry/*.json
```

Keep:
- `identity.key` (the secret).
- `events.sqlite` (the new ORM store).
- `wires/<channel>/frames.jsonl` (wire transport; not in ORM scope).
- `coordinator/accounts/*/refresh.lock` if present (filesystem
  singleflight; not in ORM scope).

A future `airc doctor --fix` will offer to delete the migrated
sidecars after verifying the ORM rows are present. Until then, hand
cleanup is the only path.

## What WON'T be migrated

- **Old peer trust** (`peers.json`). Phase 3.5 deliberately doesn't
  migrate this because the format predates the rotation-audit
  schema. Re-pair with each peer manually, or rely on the account
  registry to re-import them.
- **Old account-registry documents** (`accounts/<id>/registry.json`).
  These get re-fetched from the gh-gist on next refresh — there's
  no value in preserving a stale local cache.
- **Old `messages.jsonl`** (legacy gh bearer transport, deleted in
  #899). If your scope still has this file on disk, it's truly
  dead — `rm` it. Production traffic has used local-fs / LAN-TCP
  transport since the bearer chapter closed.

## See also

- [GRID-SUBSTRATE-AUDIT.md](architecture/GRID-SUBSTRATE-AUDIT.md)
  — the Phase 3.5 doctrine and migration history.
- [DATA-MODEL-REFERENCE.md](DATA-MODEL-REFERENCE.md) — the
  authoritative schema for every table this migration creates.
- [ACCOUNT-MESH-JOIN-CONTRACT.md](architecture/ACCOUNT-MESH-JOIN-CONTRACT.md)
  — how scopes converge on the same rooms post-migration.
