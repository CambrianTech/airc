# airc Data Model Reference

**Status**: Source of truth for every persistent table in the airc
substrate. Generated from the SeaORM entities at
`crates/airc-store/src/entities/`. If this doc disagrees with the
code, the code is right — file an issue.

**Scope**: This is the schema reference, not the access-pattern
reference. For owner/reader maps and TTL/drain policies, see the
right-hand columns; for the API surface (which `SqliteEventStore`
methods touch each table), see `crates/airc-store/src/sqlite.rs`.

## Storage layout

One SQLite file per scope, opened with WAL mode:

```
<scope-home>/events.sqlite
```

For machine-account-shared state (the wire-root home, default
`~/.airc/`), the same file lives under that root and is opened
read-write by every scope on the same user account. SQLite's WAL
journaling handles the multi-writer-one-machine case; cross-machine
sync is the account-registry document's job, not SQLite's.

Migration runner: `Migrator::up` in
`crates/airc-store/src/migration/mod.rs`. Applied automatically on
`SqliteEventStore::open`. Migrations are strictly additive — new
table or new column only, never destructive.

## Tables

### `events` — canonical transcript log

Migration `m20260519_000001_create_events`. The append log every
consumer reads from. One row per persisted `TranscriptEvent`.

| Column | Type | Notes |
|---|---|---|
| `event_id` | UUIDv4 PK | Globally unique. |
| `room_id` | UUID | Channel the event landed in. |
| `peer_id` | UUID | Author. |
| `client_id` | UUID | Runtime client tag (Claude/Codex/etc). |
| `kind` | TEXT | `TranscriptKind` as snake_case; readable in a DB browse. |
| `occurred_at_ms` | INTEGER | Wall-clock at send. |
| `lamport` | INTEGER | Sender's monotonic counter; **primary ordering key**. |
| `target` | JSON | `MentionTarget`. |
| `headers` | JSON | Header map (`airc.client`, `airc.correlation_id`, etc.). |
| `body` | JSON, NULLable | Wire payload. JSON shape is consumer-owned. |
| `attachment` | JSON, NULLable | `AttachmentManifest`. |
| `receipt` | JSON, NULLable | Delivery receipt. |
| `metadata` | JSON | Free-form consumer metadata. |

**Indexes**: composite `(room_id, lamport, event_id)` for
`page_recent` / `resume_from` O(log n + page). See migration.

**Retention**: never auto-purged. Truncation is a separate operator
verb; transcripts are durable.

**Owner**: `SqliteEventStore::append`. **Readers**: every consumer
via `page_recent` / `resume_from` / `latest_cursor`.

---

### `runtime_cursors` — per-consumer replay checkpoints

Migration `m20260522_000002_create_runtime_cursors`. Replaces the
sidecar JSON cursor files (`codex_hook_cursor.json`,
`join_feed_cursor.*.json`) that Phase 3.5 deleted.

| Column | Type | Notes |
|---|---|---|
| `consumer_id` | TEXT PK | Stable consumer tag, e.g. `join-feed:codex:thread-abc`, `codex-hook:default`. |
| `lamport` | INTEGER | Saved cursor `lamport`. |
| `event_id` | UUID | Saved cursor `event_id`. |
| `updated_at_ms` | INTEGER | Wall-clock at last save. |

**Retention**: rows persist until the consumer explicitly clears or
a future stale-cursor drain runs. No automatic eviction yet.

**Owner**: any consumer that wants resumable replay. **Readers**:
same.

---

### `peer_trust` — enrolled peer public keys

Migration `m20260522_000003_create_peer_trust`. Replaces the
deleted `peers.json` sidecar.

| Column | Type | Notes |
|---|---|---|
| `peer_id` | UUID PK | Foreign-key-equivalent to `events.peer_id`. |
| `pubkey_b64` | TEXT | URL-safe base64 (no pad). |
| `added_at_ms` | INTEGER | First enrolment timestamp. |

**Retention**: durable until explicit removal (`airc kick` /
`airc peer remove` / future).

**Owner**: pairing flows (`airc.import_invite`, account registry
import). **Readers**: signed-transport verification, `airc peers`,
account registry publishing.

---

### `peer_rotation_audit` — append-only rotation log

Migration `m20260522_000003_create_peer_trust` (same migration as
`peer_trust`).

| Column | Type | Notes |
|---|---|---|
| `peer_id` | UUID, composite PK | The rotating peer. |
| `sequence` | INTEGER, composite PK | Monotonic per-peer. Crypto-enforced. |
| `prev_pubkey_b64` | TEXT | Key being rotated out. |
| `next_pubkey_b64` | TEXT | Key being rotated in. |
| `rotated_at_ms` | INTEGER | When the rotation was signed. |
| `applied_at_ms` | INTEGER | When this scope verified + applied it. |

**Retention**: append-only forever. Trust history must be
auditable.

**Owner**: `peers_store::rotate`. **Readers**: trust audit tools,
incident response.

---

### `subscriptions` — joined channels + default-channel state

Migration `m20260522_000004_create_subscriptions`. Replaces the
deleted `subscriptions.json` and `room.json` sidecars.

| Column | Type | Notes |
|---|---|---|
| `channel_name` | TEXT PK | Human-readable channel name. |
| `room_id` | UUID | Deterministic from name + mesh identity. |
| `wire` | TEXT | Filesystem path to the wire directory. |
| `joined_at_ms` | INTEGER | First-join timestamp. |
| `is_default` | BOOLEAN | Exactly one row should have this true. |
| `parted` | BOOLEAN | Tombstone; never deleted, just flagged. |

**Per-scope**: this table is scoped — each `events.sqlite` has its
own. Joel's vHSM scope has different subscriptions than his
continuum scope.

**Retention**: durable. `airc part` flips `parted` to true and
clears `is_default` if applicable; the row stays for replay
fidelity.

**Owner**: `airc join` / `airc room`. **Readers**: every send/recv
path, `airc peer list`, `airc status`.

---

### `local_identity` — singleton install metadata

Migration `m20260522_000005_create_local_identity` (base) +
`m20260522_000009` (identity-card columns). Replaces the deleted
`identity.json` sidecar. The secret keypair stays on disk in
`identity.key` (0600); see [Identity & secrets](#identity--secrets).

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | Always 1. CHECK constraint enforces singleton. |
| `peer_id` | UUID | Stable across CLI invocations. |
| `client_id` | UUID | Runtime client default. |
| `version` | INTEGER | Schema version of this row's shape. |
| `created_at_ms` | INTEGER | First-generate timestamp. |
| `name` | TEXT | Identity card field; default `""`. |
| `pronouns` | TEXT | Identity card field; default `""`. |
| `role` | TEXT | Identity card field; default `""`. |
| `bio` | TEXT | Identity card field; default `""`. |
| `status` | TEXT | Away/back; default `""`. |
| `fingerprint` | TEXT | Cached identity fingerprint; default `""`. |
| `integrations_json` | JSON | Per-integration profile data; default `{}`. |

**Per-scope**: yes. Each scope has its own singleton row paired
with its own `identity.key`.

**Retention**: durable.

**Owner**: `airc-daemon::LocalIdentity::load_or_generate`.
**Readers**: every command that needs `peer_id` / `client_id`;
`airc identity show`.

---

### `mesh_identity` — cached account identity for room derivation

Migration `m20260522_000006_create_mesh_identity`. Replaces the
deleted `mesh_identity.json` sidecar.

| Column | Type | Notes |
|---|---|---|
| `scope` | TEXT PK | Scope home this cache entry belongs to. |
| `identity` | TEXT | Resolved identity string (`gh login`, git email, or local fallback). |
| `source` | TEXT | `GhApiUser` / `GitEmail` / `LocalHostUser` / `Operator`. |
| `resolved_at_ms` | INTEGER | When the resolver ran. |
| `ttl_ms` | INTEGER | How long this cache entry is fresh. |

**Retention**: replaced on re-resolution; `Operator`-source entries
never expire (per #868).

**Owner**: `mesh_identity::resolve` / `resolve_with`. **Readers**:
RoomId derivation, account registry document generation,
`airc whois`.

---

### `account_registry` — local cache of the published cross-machine document

Migration `m20260522_000007_create_account_registry`. Replaces the
deleted `account_registry/<mesh-identity>/registry.json` sidecar.

| Column | Type | Notes |
|---|---|---|
| `mesh_identity` | TEXT PK | The mesh this document represents. |
| `schema_version` | INTEGER | Copied from the document. |
| `generated_at_ms` | INTEGER | When the sender stamped it. |
| `document_json` | TEXT | Serialized `AccountRegistryDocument`. Wire payload. |
| `updated_at_ms` | INTEGER | When we last wrote the row. |

**Treatment**: per non-negotiable #9, the document body is opaque
JSON because it's wire payload encoding. Indexed-queryable
identifiers are typed columns.

**Retention**: upserted on every publish-or-refresh.

**Owner**: `SqliteAccountRegistryStore::publish` /
`GhAccountRegistryStore`. **Readers**: import flows on other
machines.

---

### `account_registry_gist_sentinel` — per-mesh-identity gist id

Migration `m20260522_000007_create_account_registry` (same migration).
Replaces the deleted
`<sentinel_root>/account-registry-gist-id` sidecar.

| Column | Type | Notes |
|---|---|---|
| `mesh_identity` | TEXT PK | One sentinel per mesh on this machine. |
| `gist_id` | TEXT | Opaque to the store; gh adapter interprets. |
| `updated_at_ms` | INTEGER | Last-saved timestamp. |

**Retention**: cleared by `clear_account_registry_gist_sentinel`
when the remote gist is detected as gone (out-of-band delete).

**Owner**: `GhAccountRegistryStore::save_gist_id` /
`clear_gist_id`. **Readers**: every `publish` to decide
edit-existing vs create-new.

---

### `beacons` — account-mesh presence beacons

Migration `m20260522_000008_create_beacons`. Replaces the deleted
`accounts/<mesh-identity>/beacons/<peer-id>.json` sidecar tree.

| Column | Type | Notes |
|---|---|---|
| `mesh_identity` | TEXT, composite PK | Which mesh this beacon belongs to. |
| `peer_id` | UUID, composite PK | Which peer is announcing. |
| `scope_home` | TEXT | Absolute path of the scope this beacon comes from. |
| `pid` | INTEGER | Process ID (for diagnostics; not load-bearing). |
| `published_at_ms` | INTEGER | When the beacon was first written. |
| `heartbeat_at_ms` | INTEGER | Last freshness ping. Used for live/stale partitioning. |

**Live vs stale**: a row is "live" when
`now - heartbeat_at_ms < CoordinatorConfig::heartbeat_ttl_ms`.
Drain via `drain_stale_store`. Drains are caller-driven; no
automatic eviction yet (audit TODO).

**Owner**: `coordinator::publish_store`. **Readers**:
`coordinator::snapshot_store`, account registry document
generation, peer list, status display.

---

### `beacon_channels` — channels-per-beacon many-to-many

Migration `m20260522_000008_create_beacons` (same migration).

| Column | Type | Notes |
|---|---|---|
| `mesh_identity` | TEXT, composite PK | Tied to a beacon row. |
| `peer_id` | UUID, composite PK | Tied to a beacon row. |
| `channel_name` | TEXT, composite PK | One row per channel the beacon advertised. |

**Owner**: `coordinator::publish_store` (atomic replace alongside
the beacon row). **Readers**:
`coordinator::snapshot_store::live_channels` aggregation.

## Identity & secrets

The 32-byte Ed25519 secret stays in `<scope>/identity.key` (0600 on
Unix). It is NOT in the database. The doctrine: blobs go to disk
or to OS-protected key storage (filesystem perms today; SQLCipher /
keychain / hardware enclave in production-hardened deployments).
Per non-negotiable #9, this is the deliberate exception to "durable
substrate data uses the ORM."

`local_identity.row` and `identity.key` are paired:
- Both exist → load.
- Both missing → generate.
- One missing → `PartialState` error (refuse to invent a new peer_id
  over an existing key). Recovery: see
  [PHASE-3.5-MIGRATION.md](PHASE-3.5-MIGRATION.md) (TODO).

## Cross-table invariants

- `events.peer_id` must have a `peer_trust` row (or be the local
  identity) for `VerificationPolicy::Strict` to accept the frame.
  Replay handles unknown signers via skip-and-warn (#905).
- `beacons.peer_id` similarly must be in `peer_trust` (or local) for
  the coordinator to trust it.
- `subscriptions.room_id` is deterministically derived from
  `(mesh_identity, channel_name)` via UUIDv5 — two peers that
  resolve to the same `mesh_identity` will collide on `room_id` for
  the same `channel_name`, which is how account-scoped rooms
  auto-converge.

## What's NOT in the database

- Wire transcript files (`<wire-root>/wires/<channel>/frames.jsonl`)
  — these are the **transport layer**, separate from the durable
  store. They exist so a fresh scope can replay the channel without
  needing to import a sqlite snapshot. The store is the source of
  truth for what THIS scope has seen; the wire is what it observed
  on the bus.
- Refresh-lock sentinels (`accounts/<mesh-identity>/refresh.lock`)
  — coordinator singleflight primitive. Filesystem-locked because
  the lock semantics depend on POSIX atomic rename, which SQLite
  can't model cleanly.
- The Ed25519 secret (see above).

## When to add a new table

Read non-negotiable #9 in
[GRID-SUBSTRATE-AUDIT.md](architecture/GRID-SUBSTRATE-AUDIT.md):

> Durable substrate data uses the store/ORM boundary. JSON is
> allowed for wire payload encoding, install/config bootstrap, and
> external invite documents; it is not acceptable for runtime
> cursors, trust state, subscriptions, presence registries, or
> replay checkpoints.

If your feature owns "durable substrate data" that doesn't fit any
table above, add a migration. Migration numbering is monotonic
(`m20260522_000010_*` etc.); ordering matters for the up()
sequence. Tables are strictly additive — never delete or alter
column types in an existing migration; ship a new migration that
adds a column or a sibling table instead.
