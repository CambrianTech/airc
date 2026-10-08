---
name: airc:update
description: Update AIRC through its guarded prepare, install, and verified daemon adoption path while preserving operator-stop intent.
user-invocable: true
allowed-tools: Bash
argument-hint: ""
---

# airc update

Run the supported updater when asked to update AIRC:

```bash
airc update
```

The updater prepares and verifies the artifact while service remains available,
then owns the maintenance handoff and verifies the adopted daemon build. A
current binary and daemon require no restart. A stopped account stays stopped;
ordinary update never clears durable operator-stop intent.

Do not append a manual `airc stop` / `airc join` cycle. Public stop records
operator intent, and explicit join clears it. Those are operator lifecycle
actions, not updater implementation steps. The owning account daemon is shared
by its project scopes, so a handoff can affect every scope on that account.

## Verify the result

```bash
airc version
airc status
```

Report the installed build separately from the running daemon build. If service
was intentionally stopped, an absent daemon is expected and is not a reason to
resume it. Report update, adoption, or rollback errors as failures; do not claim
that a queued message proves reader delivery.

If explicitly asked to adopt an already-installed binary and resume service,
use the existing guarded adoption command:

```bash
airc update --adopt-installed
```

Explicit `airc join` also resumes operator-stop intent. Automatic consumers use
`airc join --ensure`; it cannot clear the intent or change room/feed state.

## Failures

- Unknown or inaccessible daemon state is a refusal, not proof of absence.
- Preserve dirty source checkouts and the exact installer error; do not reset
  user work or replace the public updater with a manual rebuild/restart.
- A failed handoff is not repaired by blindly repeating stop/start. Inspect its
  typed failure and rollback result through the public lifecycle.

Aliases `airc upgrade` and `airc pull` dispatch to the same updater. Installed
skill changes are separate from binary adoption; read the updated skill when
its instructions are needed.

## Verified prebuilt preparation

For an already downloaded and checksum-verified CI/release artifact, set
`AIRC_PREBUILT_ARTIFACT` to the extracted executable and
`AIRC_PREBUILT_SHA256` to its 64-digit SHA-256, then run ordinary `airc update`.
Verify the archive's published checksum before extraction; the binary checksum
pins the extracted bytes supplied to the updater. The artifact must report the
current update channel revision. Scope these variables to this invocation and
clear them afterward.

The current installer copies the input into the updater-owned preparation
snapshot, verifies its digest before executing it, and checks its build revision.
Invalid/missing input refuses before maintenance; explicit prebuilt input never
falls back to compilation. Existing installed updater versions can use this
path because they invoke the newly fetched installer with their environment.
Maintenance, rollback, operator-stop intent and daemon revision verification
remain owned by the existing updater. Do not call installer `--prebuilt` directly
against a running installation: that phase assumes updater-owned maintenance.

Artifact discovery/download is still external to `airc update`; without explicit
prebuilt input it retains the contributor source-build behavior. This is not yet
a complete end-user release downloader.
