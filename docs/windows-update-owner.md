# Windows updater session ownership

Both `airc update` and `airc update --auto` enter the existing native installer
session before preparation, then retain that owner through daemon maintenance,
publication and verification. Native PowerShell installation continues to own
its existing session. An inherited context is accepted only after the shared
helper checks the owner PID, start time and actual ancestor chain. A stale or
mismatched update marker fails before update execution; no retry/reentry loop
creates replacement ownership.

A failed update may successfully restore and verify the previous daemon. That
specific typed Rust outcome becomes internal exit 200 only inside a validated
updater session. The session preserves that verified descendant, maps the
outcome back to public failure 1, and cleans its own elevation cache once.
Ordinary failures and cancellation keep owned process-tree cleanup. No human
error string is interpreted as proof of restoration.

Regression coverage uses the real PS5 session entry with synthetic native
gsudo/updater programs and two borrowed phases: one owner PID, one cleanup,
manual/automatic arguments, nonzero status, stale/mismatched context, and child
visibility. Unadapted Bash descendants are tested only on hosted Windows CI.
Public binary integration uses isolated IPC owners and a test-only admin probe
to prevent real credential-cache or machine setup operations.

Shared helper de1c8c0112521b3f46c357d9eaa90ba4b5280dcb is released and pinned.
Acceptance remains OPEN until exact-head CI,
fresh public binary integration, and supported live installation/rerun pass.
The final binary fixtures use the released checksum-pinned loader; they do not establish live account deployment.
