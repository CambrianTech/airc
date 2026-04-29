"""LocalBearer — message transport for same-machine peers.

When two airc tabs run on the same machine (different scopes, different
ports, but same filesystem), they currently SSH to 127.0.0.1 to talk to
each other. That works but burns SSH+crypto cycles for nothing — they
share a filesystem and can read/write each other's messages.jsonl
directly.

This bearer takes that shortcut. It serves any peer whose host_target is
a loopback address AND whose remote_home is a local writable
directory. The send path appends to the host's messages.jsonl
directly; the recv path tails the same file with pure-Python tail-F
semantics (no subprocess).

Per the bearer abstraction's adapter rule, ALL same-machine knowledge
lives in this module: the loopback-detection logic, the pure-Python
file tail loop, and the file-rotation handling. Code outside this file
never has to know "we're talking to ourselves" — the resolver picks
LocalBearer when can_serve says yes and that's the entire seam.

Why pure-Python tail rather than subprocess `tail -F`: portability
(Windows lacks tail, even Git Bash's tail behaves oddly), testability
(no Popen mock gymnastics), and lower startup cost (no fork). The
trade-off — losing tail's inotify/kqueue speed for poll-based reads —
is acceptable because LocalBearer's only path is an already-fast disk
read; we're optimizing the SSH-skip, not the file-read.
"""

from __future__ import annotations

import json
import os
import time as _time
from typing import Iterator, Optional

from .bearer import (
    Bearer,
    BearerError,
    LivenessResult,
    SendOutcome,
    ReceivedMessage,
)


class LocalBearerError(BearerError):
    """Local-transport-class errors. Distinct subclass for diagnostic
    clarity; callers branching on outcome kinds should rely on
    SendOutcome.kind, not isinstance checks."""


_LOOPBACK_HOSTS = {"127.0.0.1", "::1", "localhost", "0.0.0.0"}


def _is_loopback_target(host_target: str) -> bool:
    """Strip an optional `user@` and an optional `:port`, then test
    whether the remaining host is one of our loopback aliases.

    `host_target` shape mirrors SshBearer's: `user@host` or
    `user@host:port` or just `host`. We do NOT treat unresolvable
    hostnames as loopback — only the literal aliases above. A user
    whose hostname happens to resolve to 127.0.0.1 still gets treated
    as remote; that's safer than over-eager local routing.

    IPv6 nuance: `::1` contains colons that aren't `:port` separators.
    The standard IPv6+port form is bracketed: `[::1]:7547`. We strip
    the brackets and the trailing port; bare `::1` (no brackets) we
    accept as-is.
    """
    if not host_target:
        return False
    target = host_target.split("@", 1)[-1]  # strip user@
    # Bracketed IPv6 with optional :port — e.g. "[::1]:7547" or "[::1]".
    if target.startswith("[") and "]" in target:
        target = target[1:].split("]", 1)[0]
    elif target.count(":") == 1:
        # IPv4 or hostname with one colon = host:port. Strip port.
        target = target.rsplit(":", 1)[0]
    # else: bare IPv6 (multiple colons) or hostname-without-port — leave
    # untouched and compare against the alias set.
    return target in _LOOPBACK_HOSTS


class LocalBearer(Bearer):
    KIND = "local"

    @classmethod
    def can_serve(cls, peer_meta: dict) -> bool:
        """Serve only same-machine peers — loopback host_target AND a
        remote_home that exists as a writable directory.

        Both conditions must hold. host_target alone isn't enough
        (someone could have a stale 127.0.0.1 record from a prior
        session whose airc_home was cleaned up). remote_home alone
        isn't enough either (a path collision against a local dir
        named like a remote scope would falsely qualify). Together
        they identify "we're talking to a peer that lives in a
        directory on this machine."
        """
        host_target = peer_meta.get("host_target", "")
        if not _is_loopback_target(host_target):
            return False
        home = peer_meta.get("remote_home", "")
        if not home:
            return False
        # Expand a leading $HOME / ~ so we don't get tripped by raw env-var
        # strings the way callers serialize them. os.path.expandvars is
        # safe because we don't trust unsanitized peer_meta for command
        # execution — only for path resolution.
        expanded = os.path.expanduser(os.path.expandvars(home))
        return os.path.isdir(expanded) and os.access(expanded, os.W_OK)

    def __init__(self, peer_meta: Optional[dict] = None) -> None:
        # No IO — concrete bearers MUST be cheap to instantiate.
        self._peer_meta: dict = peer_meta or {}
        self._opened_peer_id: Optional[str] = None
        self._closed = False
        self._last_recv_ts: Optional[float] = None
        # Active recv loop state — set when recv_stream is iterating, so
        # close() can interrupt it promptly.
        self._stop_recv = False

    def _check_alive(self) -> None:
        if self._closed:
            raise LocalBearerError("bearer already closed")

    def _resolve_messages_path(self) -> str:
        home = self._peer_meta.get("remote_home", "")
        if not home:
            raise LocalBearerError(
                "LocalBearer has no remote_home in peer_meta — open() "
                "called with stale meta?"
            )
        expanded = os.path.expanduser(os.path.expandvars(home))
        return os.path.join(expanded, "messages.jsonl")

    def open(self, peer_id: str) -> None:
        """Cache peer_id. Like SshBearer, no connection is established
        here — file IO happens lazily in send() / recv_stream()."""
        self._check_alive()
        self._opened_peer_id = peer_id

    def send(self, peer_id: str, channel: str, payload: bytes) -> SendOutcome:
        """Append payload to the host's messages.jsonl directly.

        Mirrors SshBearer's payload framing — adds a trailing newline if
        absent so messages.jsonl stays strict newline-delimited regardless
        of caller framing. No __APPENDED__ confirmation needed: a successful
        os.write IS the confirmation.

        Failure mode: OSError (disk full, permission flipped, dir vanished
        between can_serve and now) → transient_failure. Caller's queue +
        retry handles it. There is no auth_failure analogue for
        LocalBearer — same-machine means same-user means same access.
        """
        self._check_alive()
        try:
            path = self._resolve_messages_path()
        except LocalBearerError as e:
            return SendOutcome(kind="transient_failure", detail=str(e))

        framed = payload if payload.endswith(b"\n") else payload + b"\n"
        try:
            # Open with O_APPEND so concurrent writers (host's own monitor,
            # other LocalBearer joiners) interleave at line boundaries.
            # POSIX guarantees writes ≤ PIPE_BUF (typically 4096) are
            # atomic for O_APPEND files; airc envelopes are well under.
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
            try:
                os.write(fd, framed)
            finally:
                os.close(fd)
        except OSError as e:
            return SendOutcome(
                kind="transient_failure",
                detail=f"local append failed: {e}",
            )
        return SendOutcome(kind="delivered", detail="")

    def recv_stream(self) -> Iterator[ReceivedMessage]:
        """Tail the host's messages.jsonl with pure-Python poll-based reads.

        Resumes from offset_file (line count) on each open so a teardown +
        reconnect doesn't replay history. Handles file rotation (size
        shrinks below the current read position) by reopening at start.
        Yields one ReceivedMessage per valid envelope; malformed lines
        are dropped silently to match the bash formatter's prior
        behavior.

        Per the bearer ABC, callers must use line-buffered IO. We always
        read line-by-line via readline() so events surface as soon as
        a \\n hits the file.
        """
        self._check_alive()
        path = self._resolve_messages_path()
        offset_file = self._peer_meta.get("offset_file")

        # Start position: lines to skip (count from beginning) so we
        # resume past what the formatter has already processed.
        skip_lines = self._compute_skip_lines(offset_file)
        consumed_lines = skip_lines
        last_inode = None

        while not self._closed and not self._stop_recv:
            try:
                f = open(path, "rb")
            except FileNotFoundError:
                # File doesn't exist yet (host hasn't started writing).
                # Brief poll until it appears or we're closed.
                self._sleep_or_break(0.5)
                continue
            try:
                # Track inode for rotation detection.
                try:
                    st = os.fstat(f.fileno())
                    last_inode = (st.st_ino, st.st_dev)
                except OSError:
                    last_inode = None

                # Skip to past the previously-consumed line count.
                for _ in range(consumed_lines):
                    if not f.readline():
                        break

                while not self._closed and not self._stop_recv:
                    line = f.readline()
                    if line:
                        consumed_lines += 1
                        self._on_line_received(consumed_lines, offset_file)
                        msg = self._parse_envelope(line)
                        if msg is None:
                            continue
                        yield msg
                        continue
                    # EOF — check for rotation, then poll briefly.
                    if self._was_rotated(path, last_inode):
                        break  # reopen below
                    self._sleep_or_break(0.1)
            finally:
                try:
                    f.close()
                except OSError:
                    pass
            # Rotation detected — reset consumed_lines so the new file
            # is read from the beginning. The offset file we'd otherwise
            # honor was for the OLD file's line count.
            if self._was_rotated(path, last_inode):
                consumed_lines = 0
                if offset_file:
                    try:
                        with open(offset_file, "w") as off:
                            off.write("0")
                    except OSError:
                        pass

    def _was_rotated(self, path: str, last_inode) -> bool:
        if last_inode is None:
            return False
        try:
            st = os.stat(path)
        except OSError:
            return True  # disappeared = treat as rotated
        return (st.st_ino, st.st_dev) != last_inode

    @staticmethod
    def _compute_skip_lines(offset_file: Optional[str]) -> int:
        """Read offset_file as a line count to skip. Same semantics as
        SshBearer._compute_tail_position but expressed as a number rather
        than a tail flag. Empty / 0 / non-numeric → start from EOF
        (skip all existing lines) by returning the current line count
        of the file.
        """
        if not offset_file:
            return _line_count_or_zero(None)  # 0; will be set to file len below
        try:
            with open(offset_file, "r") as f:
                raw = f.read().strip()
        except OSError:
            return 0
        if not raw or not raw.isdigit():
            return 0
        try:
            n = int(raw)
        except ValueError:
            return 0
        return max(0, n)

    def _on_line_received(self, line_count: int, offset_file: Optional[str]) -> None:
        """Bump last_recv_ts (for liveness) and persist offset (for resume).
        Persistence failures are swallowed — bearer keeps streaming; a
        stale offset means small replay on reconnect, which the caller
        can dedupe."""
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
        """Same envelope contract as SshBearer._parse_envelope — keep
        the formats identical so monitor_formatter doesn't need to know
        which bearer produced the line."""
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
        """Sleep in 50ms ticks so close() takes effect promptly."""
        deadline = _time.time() + seconds
        while not self._closed and not self._stop_recv and _time.time() < deadline:
            _time.sleep(0.05)

    def liveness(self, peer_id: str) -> LivenessResult:
        """Report when this bearer last received an event. Symmetric with
        SshBearer.liveness — same shape, same semantics."""
        self._check_alive()
        if self._last_recv_ts is None:
            return LivenessResult(
                peer_id=peer_id,
                last_seen_ts=None,
                bearer_diag="no events received via local tail yet",
            )
        return LivenessResult(
            peer_id=peer_id,
            last_seen_ts=self._last_recv_ts,
            bearer_diag="last event from local tail",
        )

    def close(self) -> None:
        """Idempotent. Sets the stop flag so any active recv_stream
        iteration returns at the next poll tick (within ~50ms)."""
        self._closed = True
        self._stop_recv = True
        self._opened_peer_id = None
        # peer_meta is preserved on close so post-close diagnostic reads
        # are still useful. Same convention as SshBearer.


def _line_count_or_zero(_unused) -> int:
    """Internal helper: returns 0. Phase 3a placeholder so the resume
    path's "no offset → skip nothing" reads cleanly. Future tuning
    might choose to skip to current file length here for a true
    EOF-start, matching `tail -n 0`."""
    return 0
