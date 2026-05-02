"""GhBearer — message transport over gh-gist.

When two airc peers can't reach each other directly (different LANs,
no Tailscale, asymmetric NATs), they fall back to gh-as-bearer: the
host's room gist gets a `messages.jsonl` file, peers append envelopes
to it via `gh gist edit`, and recv_stream polls the gist for new lines.

Per the bearer abstraction's adapter rule, ALL gh-as-transport knowledge
lives in this module: gh CLI invocations, gist read/parse, gist
mutation, polling cadence, rate-limit handling. The room-registry role
of gh (cmd_rooms, gistparse) lives elsewhere and is a separate concern.

Why polling rather than push: gh has no streaming API for gist updates.
Polling at a sane cadence (15s default) costs ~240 requests/hour/peer,
well under gh's 5000/hour authenticated rate limit. ETag conditional
GETs are a future optimization (saves bytes, still counts toward primary
rate limit per gh docs); deferred until rotation pressure makes the
read payload large enough that bandwidth matters.

Why subprocess `gh api` rather than direct HTTPS: gh CLI handles auth
(token discovery, refresh, GHE detection), retry on transient errors,
and surface error messages we want users to see. Reimplementing that
in Python would duplicate everything gh already does well.

Phase 3b (current): GhBearer registered AFTER SshBearer. Production
peer_meta from real pairings populates host_target (so SshBearer.can_serve
wins). GhBearer activates only for peer_meta where host_target is empty
but room_gist_id is set — a deliberate path the resolver doesn't reach
today. Phase 3c flips the order and removes SshBearer.
"""

from __future__ import annotations

import json
import os
import random as _random
import shutil
import subprocess
import sys
import tempfile
import time as _time
from typing import Iterator, Optional, Tuple

from .bearer import (
    Bearer,
    BearerError,
    LivenessResult,
    SendOutcome,
    ReceivedMessage,
)


class GhBearerError(BearerError):
    """gh-transport-class errors. Distinct subclass for diagnostic
    clarity; callers branching on outcome kinds should rely on
    SendOutcome.kind, not isinstance checks."""


_GH_BIN = "gh"
_MESSAGES_FILE = "messages.jsonl"
_DEFAULT_POLL_INTERVAL = 15.0  # seconds; tuned for gh rate limit headroom
_GH_API_TIMEOUT = 10.0          # per-call seconds; total wall time bounded by retry policy

# Rotation thresholds (gh hard limit on a gist file is 1MB; we trim
# proactively well before that so the next append always has headroom).
# An average envelope post-Phase-E is ~300-500 bytes (sig + ts + AEAD
# nonce + ciphertext); 600KB ≈ 1500-2000 envelopes per file. When we
# cross _GIST_MAX_BYTES, we keep only the last _GIST_KEEP_LINES so the
# substrate stays writable indefinitely. Older content is dropped —
# losing it is preferable to the room going write-blocked forever.
# Both can be tuned at runtime via env vars (AIRC_GIST_MAX_BYTES,
# AIRC_GIST_KEEP_LINES) for tests + power users.
_GIST_MAX_BYTES = 600_000   # rotate at 600KB (40% headroom under 1MB hard limit)
_GIST_KEEP_LINES = 1000     # keep last 1000 lines after rotation


def _resolve_gh_bin() -> str:
    """Locate gh CLI on PATH. Returns the path or raises GhBearerError.

    Inherits the user's environment (PATH, GH_TOKEN, etc) so token
    discovery, GHE host config, and proxy settings come from gh's own
    rules — not duplicated here."""
    found = shutil.which(_GH_BIN)
    if not found:
        raise GhBearerError(
            "gh CLI not found on PATH; install GitHub CLI and run 'gh auth login'"
        )
    return found


def _has_gh_auth() -> bool:
    """Return True iff `gh auth status` reports an authenticated user.

    Conservative check — used by can_serve. We do NOT raise on auth
    failure; we just decline to serve, and the resolver falls through
    to a different bearer (or PeerUnreachable if none can). This keeps
    the abstraction's "can_serve is pure inspection" invariant intact:
    one subprocess invocation, no side effects."""
    try:
        gh = _resolve_gh_bin()
    except GhBearerError:
        return False
    try:
        r = subprocess.run(
            [gh, "auth", "status"],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (subprocess.TimeoutExpired, OSError):
        return False
    return r.returncode == 0


def _classify_gh_error(combined_output: str, exit_nonzero: bool) -> str:
    """Map a `gh api` failure's stderr+stdout body to a SendOutcome.kind.

    Pattern-match on the response body / gh CLI's "(HTTP NNN)" suffix.
    The order matters: secondary_rate_limit must be checked BEFORE
    auth_failure (both can be HTTP 403, but the rate-limit text qualifies
    differently and the recovery path is "wait" not "re-auth").

    Returns one of: "secondary_rate_limit" | "gone" | "auth_failure"
                  | "transient_failure"
    """
    if not exit_nonzero:
        return "transient_failure"  # caller should not call us if exit=0
    body = (combined_output or "").lower()

    # Secondary / abuse rate limit — empirically returned as 403 with a
    # body string containing "rate limit exceeded" or "secondary rate
    # limit" (gh's REST docs:
    # https://docs.github.com/en/rest/overview/rate-limits-for-the-rest-api).
    # NOT exposed by the rate_limit endpoint, so the only signal is the
    # error body. Caller must back off LONG (90s+) — short retries
    # extend the throttle window. airc#381 forensics 2026-04-30: trip-
    # ped during a 4-peer concurrent-coord burst with primary rate at
    # 4542/5000 still nominally available.
    if "secondary rate limit" in body or "rate limit exceeded" in body:
        return "secondary_rate_limit"

    # 404 — gist deleted. Permanent. Caller MUST clear stale mapping;
    # retry will keep returning 404 forever. Distinct from auth_failure
    # because re-auth does not help; only `airc join --room <name>` to
    # re-host (or another peer's takeover) will create a new mapping.
    if "(http 404)" in body or "not found" in body:
        return "gone"

    # 401 / 403 (without the rate-limit body matched above) — auth-class
    # failure. User must re-auth (gh auth login -h github.com).
    if (
        "(http 401)" in body
        or "(http 403)" in body
        or "bad credentials" in body
        or "permission" in body
        or "401" in body
    ):
        return "auth_failure"

    # Default conservative: transient. Includes 5xx, network errors,
    # subprocess timeouts surfaced as gh's own retry-loop exhaustion.
    return "transient_failure"


def _gh_api_get(gist_id: str) -> Optional[dict]:
    """GET gists/<id> via gh api. Returns parsed JSON dict or None on
    failure (rate-limited, network blip, auth lost mid-stream).

    No retry here — caller (recv_stream's poll loop, send's read step)
    decides whether to retry or back off.

    Failure CLASSIFICATION (for callers that need to branch on 404 vs
    403-rate vs network) is done by `_gh_api_get_classified` which is a
    thin wrapper. Keep this function as the subprocess caller so existing
    test mocks of `_gh_api_get` keep working."""
    try:
        gh = _resolve_gh_bin()
    except GhBearerError as e:
        # Loud-fail per the global "evidence is for the debugger, not
        # the trash" rule. Pre-fix every silent return None below
        # masked real failures (auth lost, rate-limited, gist-gone) as
        # "transient gh hiccup, sleep+retry forever" — joiners' Monitor
        # surfaces nothing while peer chat rots in the void. Joel
        # 2026-05-02: "you fuckers use try/catch to eat errors."
        sys.stderr.write(f"[airc:bearer_gh] _gh_api_get({gist_id}): gh binary not resolvable: {e}\n")
        sys.stderr.flush()
        _gh_api_get._last_err = str(e)  # type: ignore[attr-defined]
        return None
    try:
        r = subprocess.run(
            [gh, "api", f"gists/{gist_id}"],
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT,
        )
    except (subprocess.TimeoutExpired, OSError) as e:
        sys.stderr.write(f"[airc:bearer_gh] _gh_api_get({gist_id}): subprocess error: {e}\n")
        sys.stderr.flush()
        _gh_api_get._last_err = ""  # type: ignore[attr-defined]
        return None
    if r.returncode != 0:
        combined = (r.stderr or "") + (r.stdout or "")
        sys.stderr.write(f"[airc:bearer_gh] _gh_api_get({gist_id}): gh api exit={r.returncode}: {combined.strip()[:500]}\n")
        sys.stderr.flush()
        _gh_api_get._last_err = combined  # type: ignore[attr-defined]
        return None
    try:
        return json.loads(r.stdout)
    except (ValueError, TypeError) as e:
        sys.stderr.write(f"[airc:bearer_gh] _gh_api_get({gist_id}): JSON parse failed: {e}; first 200 bytes: {(r.stdout or '')[:200]!r}\n")
        sys.stderr.flush()
        return None


def _gh_api_get_classified(gist_id: str) -> Tuple[Optional[dict], str]:
    """GET gists/<id> with failure CLASSIFICATION.

    Returns (gist_dict_or_None, kind):
      (dict, "delivered")              — success, JSON parsed
      (None, "gone")                   — 404 (gist deleted)
      (None, "secondary_rate_limit")   — 403 + rate-limit body
      (None, "auth_failure")           — 401, or 403 without rate-limit
      (None, "transient_failure")      — network, 5xx, timeout, parse,
                                         missing gh binary

    Wraps `_gh_api_get` so existing test mocks that patch _gh_api_get
    still work — the wrapper just reads the stashed _last_err sidecar
    to do classification when the underlying call returned None.

    Added 2026-04-30 (airc#381 layer A) to give send() the right kind
    to propagate."""
    # Reset sidecar before call so a stale value from a prior failure
    # doesn't bleed into a fresh attempt that returns None for some
    # other reason (test mocks that don't set _last_err).
    if hasattr(_gh_api_get, "_last_err"):
        delattr(_gh_api_get, "_last_err")
    gist = _gh_api_get(gist_id)
    if gist is not None:
        return (gist, "delivered")
    body = getattr(_gh_api_get, "_last_err", "") or ""
    if not body:
        # No sidecar — could mean test mock returned None directly.
        # Default to transient_failure (the conservative pre-#381 behavior).
        return (None, "transient_failure")
    return (None, _classify_gh_error(body, True))


def _gh_api_patch_messages_jsonl(gist_id: str, content: str) -> tuple[bool, str]:
    """PATCH gists/<id> with messages.jsonl=content via gh api.

    Returns (ok, detail). No If-Match: GitHub's gists PATCH endpoint
    EXPLICITLY rejects conditional request headers ("Conditional
    request headers are not allowed in unsafe requests unless
    supported by the endpoint"), confirmed empirically 2026-04-29.
    Concurrency control is the caller's problem (see GhBearer.send's
    verify-after-write loop).

    Failure CLASSIFICATION (gone vs secondary rate vs 409-conflict vs
    auth vs network) is done by `_gh_api_patch_classified` which is a
    thin wrapper. Keep this function as the subprocess caller so existing
    test mocks of `_gh_api_patch_messages_jsonl` keep working.

    Body is built via json.dumps so the file content's newlines /
    quotes / unicode are properly escaped — embedded literal newlines
    in the JSON string would silently 400.
    """
    try:
        gh = _resolve_gh_bin()
    except GhBearerError as e:
        return (False, str(e))
    body = json.dumps({"files": {_MESSAGES_FILE: {"content": content}}})
    try:
        r = subprocess.run(
            [gh, "api", "--method", "PATCH", f"gists/{gist_id}", "--input", "-"],
            input=body,
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT,
        )
    except (subprocess.TimeoutExpired, OSError) as e:
        return (False, f"gh api PATCH failed: {e}")
    if r.returncode == 0:
        return (True, "")
    err = (r.stderr or r.stdout or "gh api PATCH failed").strip()
    return (False, err)


def _gh_api_patch_classified(
    gist_id: str, content: str
) -> Tuple[bool, str, str]:
    """PATCH with failure CLASSIFICATION.

    Returns (ok, detail, kind):
      (True,  "",    "delivered")          — PATCH accepted (caller still
                                             must verify-after-write to
                                             catch silent clobbers)
      (False, body,  "conflict")           — HTTP 409 / "Gist cannot be
                                             updated" — concurrent write
                                             collision; caller should
                                             jittered-backoff + retry
      (False, body,  "secondary_rate_limit")
                                           — HTTP 403 with rate-limit
                                             body; back off LONG (90s+)
      (False, body,  "gone")               — HTTP 404 (gist deleted)
      (False, body,  "auth_failure")       — HTTP 401 / 403-not-rate
      (False, body,  "transient_failure")  — network, 5xx, timeout, etc

    Wraps `_gh_api_patch_messages_jsonl` so existing test mocks that
    patch the unclassified function still work.

    Added 2026-04-30 (airc#381 layer A). Splits what used to be a
    string-match-on-detail by the caller into a single classified
    return so all callers branch on the same kind taxonomy.
    """
    ok, detail = _gh_api_patch_messages_jsonl(gist_id, content)
    if ok:
        return (True, "", "delivered")

    detail_lower = detail.lower()

    # 409 conflict — distinct kind; caller retries with jittered backoff.
    # Pre-#381 this was string-matched in the caller; folding it in here
    # so all classification lives in one place.
    if "409" in detail or "cannot be updated" in detail_lower:
        return (False, detail, "conflict")

    kind = _classify_gh_error(detail, True)
    return (False, detail, kind)


def _jittered_backoff(attempt: int) -> float:
    """Exponential backoff with per-call jitter.

    Replaces the pre-#381 `0.05 * (attempt + 1)` linear schedule which
    had two failure modes:

      1. Linear progression maxes out at 0.4s after 8 attempts, total
         retry window ~1.8s. Far too short for gh's secondary rate
         limit (clears in 60-180s) — every retry hit the same throttle
         and the bearer gave up well before the window opened.

      2. Identical schedule across peers means 4 peers all retry at the
         same 0.05/0.10/0.15... offsets, amplifying the contention
         that triggered the original collision. Jitter desyncs them.

    Returns seconds to sleep. Caps at 30s per single backoff to keep
    the worst-case retry path bounded; a caller running RETRIES=8
    iterations gives a total worst-case retry window of ~60-90s with
    randomization, which IS long enough to clear secondary rate-limit
    bursts in most observed cases.

    For the secondary_rate_limit class specifically, callers should
    NOT use this function — they should sleep ~90s outright (one
    backoff burst won't clear gh's per-burst window). This function
    targets the conflict / transient classes where ~exponential growth
    fits."""
    base = 0.1 * (2 ** attempt)
    base = min(base, 30.0)
    return _random.uniform(0.5, 1.5) * base


def _rotate_if_needed(content: str) -> str:
    """Trim the gist's messages.jsonl content when approaching gh's 1MB
    file limit. Trim to a TARGET well below the trigger so we don't
    re-rotate on every subsequent append — hysteresis between high-
    and low-water marks gives the substrate breathing room.

    Per Joel 2026-04-29: "when you trim, you go PAST the number so it
    takes longer to trim again, otherwise you are constantly trimming."

    High-water (MAX_BYTES, default 600KB): when content crosses, rotate.
    Low-water (post-trim ≤ MAX_BYTES/2, ~300KB default): the rotation
    target. Gives ~300KB of headroom for the next write burst before
    the next rotation fires.

    Also caps at KEEP_LINES (default 1000) so a flood of tiny lines
    doesn't blow the line-count budget for line-oriented downstream
    consumers (formatter, offset tracking).

    All three knobs are env-tunable for tests + power users:
      AIRC_GIST_MAX_BYTES    (default 600000) — trigger
      AIRC_GIST_TARGET_BYTES (default MAX/2)  — post-trim ceiling
      AIRC_GIST_KEEP_LINES   (default 1000)   — line-count cap

    Idempotent below the trigger (returns content unchanged).
    """
    try:
        max_bytes = int(os.environ.get("AIRC_GIST_MAX_BYTES", _GIST_MAX_BYTES))
    except (TypeError, ValueError):
        max_bytes = _GIST_MAX_BYTES
    try:
        target_bytes = int(os.environ.get("AIRC_GIST_TARGET_BYTES", max_bytes // 2))
    except (TypeError, ValueError):
        target_bytes = max_bytes // 2
    try:
        keep_lines = int(os.environ.get("AIRC_GIST_KEEP_LINES", _GIST_KEEP_LINES))
    except (TypeError, ValueError):
        keep_lines = _GIST_KEEP_LINES

    # gh measures bytes, not chars; UTF-8 bytes is what counts.
    if len(content.encode("utf-8")) <= max_bytes:
        return content

    # Walk the most-recent lines backward, accumulating bytes until we
    # hit target_bytes OR keep_lines, whichever comes first. The walk
    # gives us "the latest N lines that fit in target" — exactly the
    # post-trim shape we want. Skip blanks so they don't burn the budget.
    lines = [ln for ln in content.splitlines() if ln.strip()]
    kept_reversed: list[str] = []
    bytes_so_far = 0
    for line in reversed(lines):
        # +1 for the newline we'll add on join.
        line_bytes = len(line.encode("utf-8")) + 1
        if bytes_so_far + line_bytes > target_bytes:
            break
        if len(kept_reversed) >= keep_lines:
            break
        kept_reversed.append(line)
        bytes_so_far += line_bytes
    kept = list(reversed(kept_reversed))
    return "\n".join(kept) + "\n" if kept else ""


def _read_messages_content(gist_data: dict) -> str:
    """Extract the messages.jsonl file content from a parsed gist GET
    response. Returns empty string if the file doesn't exist yet (first
    write creates it). gh api response shape:
        {"files": {"<name>": {"content": "..."}}, ...}
    """
    files = gist_data.get("files") or {}
    entry = files.get(_MESSAGES_FILE) or {}
    return entry.get("content", "") or ""


def _gh_gist_write_file(gist_id: str, content: str) -> tuple[bool, str]:
    """Write `content` as the messages.jsonl file in `gist_id`.

    Critical detail caught in production (#285): `gh gist edit GIST_ID
    file` (no flag) returns exit 0 BUT silently no-ops when the target
    filename doesn't already exist in the gist. Bug surface: bearer
    reports 'delivered', gh CLI reports success, gist is unchanged.

    Fix: read the gist's file list FIRST, then choose the correct
    subcommand:
      - file already in gist  → `gh gist edit GIST file`        (replace)
      - file NOT in gist      → `gh gist edit GIST -a file`     (add)
    The flag is required for new files. Trying plain edit first
    silently succeeds without writing — that's the trap.

    gh gist edit uses the local file's basename as the in-gist filename.
    We write to a temp file literally named messages.jsonl in a unique
    directory so the basename matches and the path is unique."""
    try:
        gh = _resolve_gh_bin()
    except GhBearerError as e:
        return (False, str(e))

    # Detect whether messages.jsonl exists in the gist BEFORE choosing
    # subcommand. Single extra GET, but eliminates the silent-no-op
    # trap. If the GET fails, default to -a (add) since that path
    # surfaces real errors when the file already exists (gh complains
    # about duplicate filename), whereas plain edit silently no-ops.
    existing = _gh_api_get(gist_id)
    file_exists_in_gist = (
        existing is not None
        and isinstance(existing.get("files"), dict)
        and _MESSAGES_FILE in existing["files"]
    )

    tmpdir = tempfile.mkdtemp(prefix="airc-ghbearer-")
    try:
        path = os.path.join(tmpdir, _MESSAGES_FILE)
        with open(path, "w") as f:
            f.write(content)
        if file_exists_in_gist:
            argv = [gh, "gist", "edit", gist_id, path]          # replace
        else:
            argv = [gh, "gist", "edit", gist_id, "-a", path]    # add new
        try:
            r = subprocess.run(
                argv, capture_output=True, text=True, timeout=_GH_API_TIMEOUT,
            )
        except (subprocess.TimeoutExpired, OSError) as e:
            return (False, f"gh gist edit failed: {e}")
        if r.returncode == 0:
            return (True, "")
        # Defense: if our existence check disagreed with reality (race —
        # another peer added the file between our GET and our edit),
        # try the OTHER subcommand once before giving up.
        alt_argv = (
            [gh, "gist", "edit", gist_id, path] if not file_exists_in_gist
            else [gh, "gist", "edit", gist_id, "-a", path]
        )
        try:
            r2 = subprocess.run(
                alt_argv, capture_output=True, text=True, timeout=_GH_API_TIMEOUT,
            )
        except (subprocess.TimeoutExpired, OSError) as e:
            return (False, f"gh gist edit retry failed: {e}")
        if r2.returncode == 0:
            return (True, "")
        err = (r.stderr or r.stdout or "gh gist edit failed").strip()
        return (False, err)
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


class GhBearer(Bearer):
    KIND = "gh"

    @classmethod
    def can_serve(cls, peer_meta: dict) -> bool:
        """Serve any peer with a room_gist_id and a working gh auth.

        Why room_gist_id rather than peer_id-derived addressing: gh-as-
        bearer uses the SHARED room gist for the substrate's message
        log. Every peer in the room reads/writes the same gist file.
        This matches how IRC works on a server — the channel is the
        bearer's addressable surface, not individual peers.
        """
        if not peer_meta.get("room_gist_id"):
            return False
        return _has_gh_auth()

    def __init__(self, peer_meta: Optional[dict] = None) -> None:
        # No IO — concrete bearers MUST be cheap to instantiate.
        # Even can_serve's gh auth check is run by the resolver, not
        # by us at construction time.
        self._peer_meta: dict = peer_meta or {}
        self._opened_peer_id: Optional[str] = None
        self._closed = False
        self._last_recv_ts: Optional[float] = None
        # Tracks how many lines of messages.jsonl we've already yielded.
        # Resumed from offset_file on first poll if available.
        self._consumed_lines: int = 0
        # Polling cadence; can be overridden via peer_meta for tests.
        self._poll_interval: float = float(
            peer_meta.get("poll_interval", _DEFAULT_POLL_INTERVAL)
        ) if peer_meta else _DEFAULT_POLL_INTERVAL

    def _check_alive(self) -> None:
        if self._closed:
            raise GhBearerError("bearer already closed")

    def open(self, peer_id: str) -> None:
        """Cache peer_id; no IO. Like SshBearer / LocalBearer, gh-as-
        bearer is connectionless from the bearer's POV — every send is
        a discrete gh API round-trip, every recv-poll is independent."""
        self._check_alive()
        self._opened_peer_id = peer_id
        # Initialize consumed_lines from offset_file if provided. Only
        # done once at open (not at each poll) so an in-flight reconnect
        # doesn't reset to disk state mid-stream.
        offset_file = self._peer_meta.get("offset_file")
        self._consumed_lines = self._read_offset(offset_file)

    def send(self, peer_id: str, channel: str, payload: bytes) -> SendOutcome:
        """Append `payload` to the room gist's messages.jsonl file with
        ETag-conditional concurrency control.

        Concurrency model: GitHub's gists PATCH endpoint rejects
        conditional headers (If-Match → 400) so optimistic-locking via
        ETag isn't an option. Instead: read-modify-PATCH loop with two
        conflict-detection paths: (1) explicit HTTP 409 from gh ("Gist
        cannot be updated") → retry; (2) verify-after-write — re-GET
        and check our line is in the post-write content; if not,
        another peer's PATCH clobbered ours silently → retry. Both
        paths bounded by RETRIES.

        Outcome kinds:
          delivered             — PATCH succeeded and verify saw our line
          transient_failure     — network / 5xx / conflict-after-retries
          gone                  — gist returned 404 (deleted permanently);
                                  caller MUST clear stale mapping
          secondary_rate_limit  — gh threw 403 with rate-limit body;
                                  back off LONG (90s+) before next attempt
          auth_failure          — 401, or 403 not matching rate-limit body

        airc#381 layer A (2026-04-30) split the pre-existing single
        "transient_failure" catch-all into the four real classes above.
        Old code returned auth_failure on 404 ("permission/401/not
        found/404" matched together) which was wrong for two reasons:
        (a) re-auth doesn't bring back a deleted gist, and (b) it sent
        users to `gh auth login` for a remedy that wouldn't help.
        """
        self._check_alive()

        gist_id = self._peer_meta.get("room_gist_id")
        if not gist_id:
            raise GhBearerError(
                f"GhBearer.send called for peer_id={peer_id!r} with no "
                f"room_gist_id in peer_meta — open() called with stale meta?"
            )

        framed = payload if payload.endswith(b"\n") else payload + b"\n"
        try:
            framed_str = framed.decode("utf-8")
        except UnicodeDecodeError:
            return SendOutcome(
                kind="transient_failure",
                detail="payload is not utf-8; gh-bearer requires text envelopes",
            )

        # Concurrency strategy: retry on BOTH explicit 409 conflicts
        # AND silent-clobber detected via verify-after-write. continuum-
        # b741 caught HTTP 409 "Gist cannot be updated" 4/5 times on a
        # 5-way concurrent burst (#299) — gh's PATCH endpoint returns
        # 409 when a parallel update commits between our GET and our
        # PATCH. Pre-fix code returned transient_failure on first 409
        # without retry, hence the 80% loss rate. Now: 409 → loop. Also
        # keep verify-after-write as a second-line defense for the rarer
        # silent-clobber path (PATCH returns 200 but our line isn't in
        # the post-write content).
        #
        # Backoff schedule (2026-04-30 fix, airc#381 layer A): replaced
        # `0.05 * (attempt + 1)` with `_jittered_backoff(attempt)` which
        # is exponential + per-call randomized. Old linear schedule
        # was identical across peers, so 4 peers all retried at the
        # same offsets and amplified contention; jittered exponential
        # desyncs them and gives a meaningfully longer total retry
        # window (~60-90s vs ~1.8s).
        RETRIES = 8
        last_detail = ""
        for attempt in range(RETRIES):
            gist, get_kind = _gh_api_get_classified(gist_id)
            if gist is None:
                # GET failed — distinguish permanent (gone) / wait (rate)
                # / re-auth (auth) / retry (transient). Old code coalesced
                # all into transient_failure; new path propagates the
                # right kind so caller takes the right recovery action.
                if get_kind in ("gone", "secondary_rate_limit", "auth_failure"):
                    return SendOutcome(
                        kind=get_kind,
                        detail=f"GET gists/{gist_id} failed: {get_kind}",
                    )
                # transient — caller will queue + retry; no point
                # spinning the inner loop on this attempt.
                return SendOutcome(
                    kind="transient_failure",
                    detail=f"could not fetch gist {gist_id} (network/5xx/timeout)",
                )
            existing = _read_messages_content(gist)
            new_content = _rotate_if_needed(existing) + framed_str

            ok, detail, patch_kind = _gh_api_patch_classified(gist_id, new_content)
            if not ok:
                last_detail = detail
                # Conflict is the only case where we loop — every other
                # class is structurally not retryable on this attempt
                # (gone won't un-gone, rate-limit won't clear in 1s,
                # auth won't unfail without user intervention).
                if patch_kind == "conflict":
                    _time.sleep(_jittered_backoff(attempt))
                    continue
                # gone / secondary_rate_limit / auth_failure /
                # transient_failure all propagate as-is.
                return SendOutcome(kind=patch_kind, detail=detail)

            # PATCH said OK — verify-after-write to catch silent clobbers
            # (rare; gh sometimes accepts our PATCH even when it
            # overwrote a racer's commit).
            verify, _verify_kind = _gh_api_get_classified(gist_id)
            if verify is None:
                # Verify GET failed — assume delivered (PATCH said ok).
                # Treating a verify-failure as delivered is safer than
                # treating it as not-delivered: a false-positive duplicate
                # is recoverable (verify-after-write next iteration on the
                # caller side will dedup), a false-negative loss is not.
                return SendOutcome(kind="delivered", detail="")
            if framed_str.rstrip("\n") in _read_messages_content(verify):
                return SendOutcome(kind="delivered", detail="")
            last_detail = "verify-after-write: line not in gist post-PATCH (silent clobber)"
            _time.sleep(_jittered_backoff(attempt))

        return SendOutcome(
            kind="transient_failure",
            detail=f"concurrent-write conflict after {RETRIES} retries; last: {last_detail}",
        )

    def recv_stream(self) -> Iterator[ReceivedMessage]:
        """Poll the room gist on a cadence; yield new envelopes.

        Each poll:
          1. GET gists/<id> via gh api
          2. Split content of messages.jsonl into lines
          3. For lines past self._consumed_lines, parse as envelope,
             yield. Bump consumed_lines + offset file.
          4. Sleep poll_interval (default 15s), repeat.

        On gh API failure (rate limit, network blip), we sleep the same
        cadence and try again. The bearer's job is to keep producing
        events; the caller's watchdog observes extended silence via
        liveness().
        """
        self._check_alive()

        gist_id = self._peer_meta.get("room_gist_id")
        if not gist_id:
            raise GhBearerError(
                "GhBearer.recv_stream called with no room_gist_id in peer_meta"
            )
        offset_file = self._peer_meta.get("offset_file")

        while not self._closed:
            gist = _gh_api_get(gist_id)
            if gist is None:
                # Transient gh API failure. Sleep + retry. Caller's
                # watchdog observes extended silence and escalates.
                self._sleep_or_break(self._poll_interval)
                continue
            content = _read_messages_content(gist)
            # splitlines() on the str preserves multi-byte sequences and
            # correctly handles trailing-newline absence. We re-encode
            # each line to bytes for ReceivedMessage.payload symmetry
            # with SshBearer/LocalBearer (which produce bytes from disk).
            lines = content.splitlines()
            # Shrink/rotation/clobber recovery: if our resume offset is
            # ahead of the current gist content, the gist must have been
            # truncated since we last polled (rotation hit, peer
            # clobbered the file with a bad PATCH, host self-evicted +
            # republished). Pre-2026-04-29 this stuck the bearer
            # forever — the for-range was empty, no yield ever fired,
            # the channel went dead-silent, the user saw "frozen"
            # monitors. Resync to the current end so future appends are
            # picked up.
            if self._consumed_lines > len(lines):
                self._consumed_lines = len(lines)
                self._on_line_received(self._consumed_lines, offset_file)
            for idx in range(self._consumed_lines, len(lines)):
                raw = lines[idx].encode("utf-8")
                self._consumed_lines = idx + 1
                self._on_line_received(self._consumed_lines, offset_file)
                msg = self._parse_envelope(raw)
                if msg is None:
                    continue
                yield msg
                if self._closed:
                    return
            if self._closed:
                return
            self._sleep_or_break(self._poll_interval)

    @staticmethod
    def _read_offset(offset_file: Optional[str]) -> int:
        """Read offset_file as a line count; 0 on empty/invalid/missing."""
        if not offset_file:
            return 0
        try:
            with open(offset_file, "r") as f:
                raw = f.read().strip()
        except OSError:
            return 0
        if not raw or not raw.isdigit():
            return 0
        try:
            return max(0, int(raw))
        except ValueError:
            return 0

    def _on_line_received(self, line_count: int, offset_file: Optional[str]) -> None:
        """Bump last_recv_ts (for liveness) and persist offset (for resume).
        Persistence failures are swallowed — the bearer keeps streaming."""
        self._last_recv_ts = _time.time()
        if offset_file is None:
            return
        try:
            with open(offset_file, "w") as f:
                f.write(str(line_count))
        except OSError:
            pass

    @staticmethod
    def _parse_envelope(raw_line: bytes) -> Optional[ReceivedMessage]:
        """Same envelope contract as SshBearer / LocalBearer. Keeping
        the parse identical lets monitor_formatter consume any bearer's
        output uniformly."""
        line = raw_line.rstrip(b"\n").rstrip(b"\r")
        if not line:
            return None
        try:
            env = json.loads(line)
        except (ValueError, TypeError):
            return None
        if not isinstance(env, dict):
            return None
        sender = env.get("from")
        channel = env.get("channel", "")
        if not sender:
            return None
        return ReceivedMessage(
            sender_peer_id=str(sender),
            channel=str(channel),
            payload=line,
            bearer_metadata={"envelope": env},
        )

    def _sleep_or_break(self, seconds: float) -> None:
        """Sleep in 100ms ticks so close() takes effect within ~100ms."""
        deadline = _time.time() + seconds
        while not self._closed and _time.time() < deadline:
            _time.sleep(0.1)

    def liveness(self, peer_id: str) -> LivenessResult:
        """Last_recv_ts bumped on each yielded event. Same shape as
        SshBearer / LocalBearer so cross-process consumers (airc status)
        treat all bearers uniformly."""
        self._check_alive()
        if self._last_recv_ts is None:
            return LivenessResult(
                peer_id=peer_id,
                last_seen_ts=None,
                bearer_diag="no events received via gh poll yet",
            )
        return LivenessResult(
            peer_id=peer_id,
            last_seen_ts=self._last_recv_ts,
            bearer_diag="last event from gh poll",
        )

    def close(self) -> None:
        """Idempotent. Sets the close flag so any active poll loop
        returns at the next sleep tick (within ~100ms)."""
        self._closed = True
        self._opened_peer_id = None
