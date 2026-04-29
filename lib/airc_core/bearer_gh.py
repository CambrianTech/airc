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
import shutil
import subprocess
import tempfile
import time as _time
from typing import Iterator, Optional

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


def _gh_api_get(gist_id: str) -> Optional[dict]:
    """GET gists/<id> via gh api. Returns parsed JSON dict or None on
    failure (rate-limited, network blip, auth lost mid-stream).

    No retry here — caller (recv_stream's poll loop, send's read step)
    decides whether to retry or back off."""
    try:
        gh = _resolve_gh_bin()
    except GhBearerError:
        return None
    try:
        r = subprocess.run(
            [gh, "api", f"gists/{gist_id}"],
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if r.returncode != 0:
        return None
    try:
        return json.loads(r.stdout)
    except (ValueError, TypeError):
        return None


def _gh_api_get_with_etag(gist_id: str) -> Optional[tuple[dict, str]]:
    """Like _gh_api_get but also returns the ETag for conditional PATCH.

    Implementation: `gh api -i` includes response headers in stdout
    before the JSON body (RFC 7230 — headers, blank line, body).
    Parse the ETag header; the body is everything after the first
    blank line.

    Returns (gist_dict, etag) on success, None on any failure. Empty
    etag string is acceptable — caller will skip If-Match and accept
    the lost-write risk; that mirrors pre-2026-04-29 behavior, used
    only when this helper degrades gracefully on older gh CLI versions
    that don't surface headers cleanly.
    """
    try:
        gh = _resolve_gh_bin()
    except GhBearerError:
        return None
    try:
        r = subprocess.run(
            [gh, "api", "-i", f"gists/{gist_id}"],
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if r.returncode != 0:
        return None
    head, _, body = r.stdout.partition("\r\n\r\n")
    if not body:
        head, _, body = r.stdout.partition("\n\n")
    etag = ""
    for line in head.splitlines():
        if line.lower().startswith("etag:"):
            etag = line.split(":", 1)[1].strip()
            break
    try:
        return (json.loads(body), etag)
    except (ValueError, TypeError):
        return None


def _gh_api_patch_messages_jsonl(
    gist_id: str, content: str, etag: str
) -> tuple[bool, int, str]:
    """PATCH gists/<id> with messages.jsonl=content and If-Match: <etag>.

    Returns (ok, http_status, detail).
      ok=True              — write landed (status 200)
      ok=False, status=412 — conflict, ETag stale, caller retries
      ok=False, other      — fatal-ish, caller surfaces

    Empty etag → unconditional write (no If-Match header). That's the
    fallback when _gh_api_get_with_etag couldn't parse the ETag; same
    last-writer-wins risk as pre-fix, but at least the call still
    works. Loud-fail would be worse here than degrade.
    """
    try:
        gh = _resolve_gh_bin()
    except GhBearerError as e:
        return (False, 0, str(e))
    body = json.dumps({"files": {_MESSAGES_FILE: {"content": content}}})
    argv = [gh, "api", "--method", "PATCH", "-i", f"gists/{gist_id}", "--input", "-"]
    if etag:
        argv += ["-H", f"If-Match: {etag}"]
    try:
        r = subprocess.run(
            argv,
            input=body,
            capture_output=True,
            text=True,
            timeout=_GH_API_TIMEOUT,
        )
    except (subprocess.TimeoutExpired, OSError) as e:
        return (False, 0, f"gh api PATCH failed: {e}")
    # Parse status line from headers (HTTP/1.1 <code> <msg>\r\n...).
    head = r.stdout.split("\r\n\r\n", 1)[0] if "\r\n\r\n" in r.stdout else \
           r.stdout.split("\n\n", 1)[0]
    status = 0
    for line in head.splitlines():
        if line.startswith("HTTP/"):
            parts = line.split()
            if len(parts) >= 2 and parts[1].isdigit():
                status = int(parts[1])
            break
    if r.returncode == 0 and 200 <= status < 300:
        return (True, status, "")
    err = (r.stderr or r.stdout or "gh api PATCH failed").strip()
    return (False, status, err)


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

        Pre-2026-04-29 this was a naive GET-then-PUT race: two peers
        chattering at the same time would each read the same content,
        each append their own line, each PUT the result; last writer
        won, the other's line silently vanished. continuum-b741 caught
        only-1-of-3 PONGs reaching the gist as the highest-impact
        symptom (#299), but every concurrent broadcast suffered the
        same loss class.

        Now: GET captures the gist's ETag, PATCH carries `If-Match: <etag>`.
        On 412 Precondition Failed (another peer wrote first), retry up
        to RETRIES times — each retry re-reads, so the merge keeps both
        the racer's line AND ours. Below the chat-pace traffic level a
        single retry suffices; bound the loop so a hot room doesn't
        livelock.

        Outcome kinds:
          delivered          — PATCH succeeded
          transient_failure  — read failed, write failed, network blip,
                               rate limit, retries exhausted on conflict
          auth_failure       — gh auth status currently fails (the
                               can_serve gate caught a stale state, but
                               token expired between can_serve and now)
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

        RETRIES = 4
        last_detail = ""
        for attempt in range(RETRIES):
            result = _gh_api_get_with_etag(gist_id)
            if result is None:
                # GET-with-headers failed; fall back to plain GET +
                # unconditional PATCH (degraded mode, last-writer-wins).
                gist = _gh_api_get(gist_id)
                if gist is None:
                    return SendOutcome(
                        kind="transient_failure",
                        detail=f"could not fetch gist {gist_id} (rate limit, network, or auth)",
                    )
                etag = ""
            else:
                gist, etag = result

            existing = _read_messages_content(gist)
            new_content = _rotate_if_needed(existing) + framed_str

            ok, status, detail = _gh_api_patch_messages_jsonl(gist_id, new_content, etag)
            if ok:
                return SendOutcome(kind="delivered", detail="")
            last_detail = detail
            if status == 412:
                # Another peer wrote between our GET and PATCH — retry
                # with fresh ETag. Tiny backoff so concurrent retriers
                # don't lockstep.
                _time.sleep(0.05 * (attempt + 1))
                continue
            lower = detail.lower()
            if "permission" in lower or status == 401 or "not found" in lower or status == 404:
                return SendOutcome(kind="auth_failure", detail=detail)
            return SendOutcome(kind="transient_failure", detail=detail)

        return SendOutcome(
            kind="transient_failure",
            detail=f"ETag conflict after {RETRIES} retries (room very busy?); last: {last_detail}",
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
