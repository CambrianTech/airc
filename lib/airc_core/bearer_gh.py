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
        """Append `payload` to the room gist's messages.jsonl file.

        Read-modify-write via gh CLI: GET current content, append our
        line, edit the gist with combined content. Optimistic concurrency:
        if two peers race, the loser's write OVERWRITES the winner's.
        Real fix is ETag/If-Match (gh CLI doesn't expose this directly);
        deferred to a follow-up — the conflict window is sub-second and
        rare in practice for chat-pace traffic.

        Outcome kinds:
          delivered          — gh edit succeeded
          transient_failure  — read failed, write failed, network blip,
                               rate limit, gh auth lost mid-call
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

        gist = _gh_api_get(gist_id)
        if gist is None:
            # Most common cause: rate-limited or transient gh API error.
            # Auth-lost is a sub-case; we don't try to disambiguate
            # here because caller's queue+retry handles both equally.
            return SendOutcome(
                kind="transient_failure",
                detail=f"could not fetch gist {gist_id} (rate limit, network, or auth)",
            )

        framed = payload if payload.endswith(b"\n") else payload + b"\n"
        try:
            framed_str = framed.decode("utf-8")
        except UnicodeDecodeError:
            return SendOutcome(
                kind="transient_failure",
                detail="payload is not utf-8; gh-bearer requires text envelopes",
            )
        new_content = _read_messages_content(gist) + framed_str

        ok, detail = _gh_gist_write_file(gist_id, new_content)
        if ok:
            return SendOutcome(kind="delivered", detail="")
        # gh returns "permission denied" or "404" for auth issues.
        # Treat those as auth_failure so the caller surfaces them
        # loudly rather than queueing forever.
        lower = detail.lower()
        if "permission" in lower or "401" in lower or "not found" in lower:
            return SendOutcome(kind="auth_failure", detail=detail)
        return SendOutcome(kind="transient_failure", detail=detail)

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
