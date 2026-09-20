# Durable work submissions

`WorkEvent::WorkSubmitted` records a candidate independently of its producer's
filesystem. `Airc::submit_work_in` supplies the publisher identity and publication
time; callers supply a stable submission ID, card and claim IDs, instance, full
base commit ID, and an existing `MediaRef` (SHA-256, byte count, optional MIME).

The board holds references, never patch bytes. Large artifacts must use the blob
path; a 256 KiB inline patch is incompatible with the current WebRTC frame budget
and would multiply across board pages. This slice does **not** implement remote
blob transfer. Publication success means a reference was emitted, not that a
receiver fetched or verified its bytes, or that final claim arbitration accepted
the submission. Consumers must inspect the projected candidate and verify fetched
bytes against both digest and size before applying them.

Replay checks the publisher against the transcript author and admits a new
candidate only for the active claim holder on an unsettled card, before the
recorded lease expiry. These are historical checks using the publication time,
not the time at which a restarted reader rebuilds its board. Publication times
remain publisher assertions; this is not a trusted-clock or benchmark proof.

Accepted references appear newest-first in canonical transcript order. Reusing
an ID with identical immutable fields is an idempotent retry, even with a later
publication time or after settlement; the first accepted timestamp remains.
Reusing an ID for different content records a conflict without replacing it.
Reassignment does not rewrite earlier candidate authorship.

Semantic rejection is visible in `last_submission_rejection` and `airc work
board`. Malformed submission bodies are skipped with an event-ID diagnostic so
one invalid submission cannot make the board unreadable. The JSON discriminator
takes precedence over the optional kind header; a spoofed header cannot hide
malformed events from other families. The original transcript remains the audit
record. An internal rejection event cannot be accepted from the wire.

## Reader rollout

The board cache format is bumped to force a cold replay. Old serialized cards
still deserialize with empty submission history, but older event readers do not
understand `work_submitted`. Upgrade participating readers before enabling a
producer; adding a variant to the existing v1 event family is not rolling-reader
compatibility. No live daemon is restarted or producer enabled by this change.

The next slices must implement portable blob retrieval, publish at settlement,
bind grading and feedback to the accepted submission identity, and prove transfer
and application on a second machine. A reference alone is not that proof.
