"""SshBearer — message transport over SSH.

ALL SSH-specific knowledge lives in this module: ssh binary location, key
selection, host/port resolution, MSYS path handling on Windows, the
`__APPENDED__` confirmation protocol, error classification (auth vs
network), Tailscale-CGNAT offline-detection fast-path. Code outside this
file does not mention SSH or Tailscale.

If a future contributor needs to find "how does airc do SSH," the answer
is "open this file." If they need to add a new transport (gh, Reticulum,
LoRa, websocket, anything), they write a sibling file in the same shape
and register it in bearer_resolver.py. They never touch this one.

Phase 1 (current state): send() is functional. The cmd_send.sh SSH
delivery primitive — including the relay_ssh subprocess invocation, the
__APPENDED__ confirmation, the Tailscale-offline fast-path, and the
auth/network error classification — has been relocated here. cmd_send.sh
calls this module via bearer_cli.

Phase 2 (next): recv_stream() relocates the monitor's SSH-tail logic.
liveness() relocates the heartbeat read.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
from typing import Iterator, Optional

from .bearer import (
    Bearer,
    BearerError,
    LivenessResult,
    SendOutcome,
    PeerUnreachable,
    ReceivedMessage,
)


class SshBearerError(BearerError):
    """SSH-transport-class errors. Distinct subclass for diagnostic clarity;
    callers branching on outcome kinds (SendOutcome.kind) should not
    isinstance-check this — the outcome contract is the API."""


# Tailscale CGNAT range (100.64.0.0/10): hosts whose IPs fall here come
# via Tailscale and the local `tailscale status` can tell us if they're
# offline before we waste a 10s SSH ConnectTimeout. Ranges 100.64–100.127.
_CGNAT_RE = re.compile(
    r"^100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\."
)

# Default SSH options — match the prior relay_ssh defaults exactly so
# behavior is preserved across the bash→Python relocation.
#   StrictHostKeyChecking=accept-new — TOFU on first contact, refuse on key change
#   ConnectTimeout=10                — fail fast on unreachable hosts
#   ServerAliveInterval=30           — keep long-lived monitor tails alive
_SSH_OPTS = [
    "-o", "StrictHostKeyChecking=accept-new",
    "-o", "ConnectTimeout=10",
    "-o", "ServerAliveInterval=30",
]


def _resolve_ssh_bin() -> str:
    """Locate ssh on PATH. Inherits the user's environment so platform
    quirks (Git Bash on Windows, /usr/bin/ssh on macOS, etc.) resolve
    naturally. Raises SshBearerError if no ssh is found."""
    bin_path = shutil.which("ssh")
    if not bin_path:
        raise SshBearerError(
            "ssh binary not found on PATH; install OpenSSH or Git for Windows"
        )
    return bin_path


def _resolve_tailscale_bin() -> Optional[str]:
    """Locate tailscale CLI if installed. Returns None when absent —
    the offline fast-path simply doesn't engage. Tailscale is the ONE
    transport we still know about by name in this module; it's the SSH
    bearer's optimization for CGNAT hosts. After Phase 3 (Tailscale
    dropped), this function and the fast-path it gates can be deleted
    in a single edit."""
    return shutil.which("tailscale")


def _is_peer_offline_in_tailnet(host_target: str) -> bool:
    """Confirm the peer is reported offline by local tailscale status.

    Returns True ONLY when we have positive confirmation of offline
    state. Returns False for: online, unknown, non-CGNAT targets, or
    any error reading tailscale state. Never raises — uncertainty is
    "False" so the caller falls through to the normal SSH attempt.

    Mirrors the prior bash function (airc:510). Strips a leading
    `user@` from host_target before the CGNAT check (issue #78 root
    cause: resume paths fed in `user@host` and silently bypassed the
    gate)."""
    if not host_target:
        return False
    # Strip user@ prefix if present.
    host = host_target.split("@", 1)[-1]
    if not _CGNAT_RE.match(host):
        return False
    ts_bin = _resolve_tailscale_bin()
    if not ts_bin:
        return False
    try:
        result = subprocess.run(
            [ts_bin, "status"],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (subprocess.TimeoutExpired, OSError):
        return False
    if result.returncode != 0:
        return False
    # tailscale status format: "<IP>  <hostname>  <owner>  <os>  <state...>"
    # Match target IP at column 1 + the literal word "offline" anywhere on
    # the same line.
    for line in result.stdout.splitlines():
        cols = line.split()
        if not cols:
            continue
        if cols[0] == host and "offline" in line:
            return True
    return False


def _classify_ssh_failure(stderr: str) -> tuple[str, str]:
    """Categorize an ssh failure based on its stderr.

    Returns (kind, detail) where kind is one of "auth_failure",
    "transient_failure". Auth failures are fatal-until-repair (user
    must re-pair); transient failures are retryable.

    The pre-existing bash code distinguished these via grep on stderr
    — this is the same distinction in Python. Keeping the strings
    literal so behavior is preserved.
    """
    auth_markers = [
        "Permission denied",
        "Authentication failed",
        "publickey",
    ]
    if any(m in stderr for m in auth_markers):
        return ("auth_failure", "host refused our SSH identity; re-pair required")
    return ("transient_failure", stderr.strip().splitlines()[-1] if stderr.strip() else "ssh failed")


def _build_ssh_argv(host_target: str, identity_key: Optional[str], remote_cmd: str) -> list[str]:
    """Construct the argv for a single ssh invocation. host_target is
    `user@host` or `user@host:port`. Identity key is optional — if
    provided we pass `-i`; otherwise ssh uses its default key search.

    Splits user@host:port into user@host plus a separate -p port
    argument (ssh's CLI doesn't accept :port in the host arg)."""
    ssh_bin = _resolve_ssh_bin()
    argv = [ssh_bin]
    if identity_key:
        argv += ["-i", identity_key]
    argv += list(_SSH_OPTS)
    # Split off port if present.
    target = host_target
    if ":" in target:
        target, port = target.rsplit(":", 1)
        argv += ["-p", port]
    argv.append(target)
    argv.append(remote_cmd)
    return argv


class SshBearer(Bearer):
    KIND = "ssh"

    @classmethod
    def can_serve(cls, peer_meta: dict) -> bool:
        """Return True if peer_meta describes an SSH-reachable peer.

        SSH reachability requires a `host_target` field (user@host[:port])
        populated by the pair-handshake. peer_meta is supplied by the
        caller; the disk-side identity-key check is lazy in send().
        """
        return bool(peer_meta.get("host_target"))

    def __init__(self, peer_meta: Optional[dict] = None) -> None:
        # No IO — concrete bearers MUST be cheap to instantiate.
        # peer_meta supplied by the resolver. Optional for unit-test
        # ergonomics (tests construct directly without a resolver).
        self._opened_peer_id: Optional[str] = None
        self._peer_meta: dict = peer_meta or {}
        self._closed = False

    def _check_alive(self) -> None:
        if self._closed:
            raise SshBearerError("bearer already closed")

    def open(self, peer_id: str) -> None:
        """Cache peer_id for subsequent send() calls. No actual SSH
        connection is established at open() — SSH is connectionless from
        the bearer's POV (each send is one ssh invocation). Per the ABC,
        open() may legitimately be a near-no-op for transports that don't
        need a persistent connection."""
        self._check_alive()
        self._opened_peer_id = peer_id

    def send(self, peer_id: str, channel: str, payload: bytes) -> SendOutcome:
        """Deliver `payload` to `peer_id` over SSH, append to the host's
        messages.jsonl, confirm via __APPENDED__ marker.

        Mirrors the cmd_send.sh:194-228 primitive precisely; behavior is
        preserved across the relocation. The Tailscale-offline fast-path
        engages first to skip predictable misses; on attempt, stderr is
        inspected to classify auth vs transient failures."""
        self._check_alive()

        host_target = self._peer_meta.get("host_target")
        if not host_target:
            raise SshBearerError(
                f"SshBearer.send called for peer_id={peer_id!r} with no "
                f"host_target in peer_meta — open() called with stale meta?"
            )
        remote_home = self._peer_meta.get("remote_home", "$HOME/.airc")
        identity_key = self._peer_meta.get("identity_key")

        # Fast-path: known-offline tailnet peer. Queue immediately; the
        # caller's monitor flush_pending_loop drains when the peer wakes.
        if _is_peer_offline_in_tailnet(host_target):
            return SendOutcome(
                kind="queued_unreachable",
                detail=f"peer offline in tailnet, auto-delivers on wake",
            )

        # Normal SSH attempt: append to remote messages.jsonl, confirm via
        # the __APPENDED__ marker. Trust the marker over ssh's exit code —
        # some shells bubble benign stderr warnings up as nonzero exit
        # even when the append succeeded.
        remote_cmd = f"cat >> {remote_home}/messages.jsonl && echo __APPENDED__"
        argv = _build_ssh_argv(host_target, identity_key, remote_cmd)

        # Payload is opaque bytes; the prior bash path used a trailing newline
        # via `printf '%s\n'`. Preserve that to keep messages.jsonl a strict
        # newline-delimited JSON file regardless of caller payload framing.
        stdin_bytes = payload if payload.endswith(b"\n") else payload + b"\n"

        try:
            result = subprocess.run(
                argv,
                input=stdin_bytes,
                capture_output=True,
                timeout=15,  # 10s connect + buffer for the cat append
            )
        except subprocess.TimeoutExpired:
            return SendOutcome(
                kind="transient_failure",
                detail="ssh timed out after 15s",
            )
        except OSError as e:
            return SendOutcome(
                kind="transient_failure",
                detail=f"ssh exec failed: {e}",
            )

        stdout = result.stdout.decode("utf-8", errors="replace")
        stderr = result.stderr.decode("utf-8", errors="replace")

        if "__APPENDED__" in stdout:
            return SendOutcome(kind="delivered", detail="")

        # Failure path: classify by stderr.
        kind, detail = _classify_ssh_failure(stderr)
        return SendOutcome(kind=kind, detail=detail)

    def recv_stream(self) -> Iterator[ReceivedMessage]:
        self._check_alive()
        raise NotImplementedError(
            "SshBearer.recv_stream is Phase 2 work; the monitor still does "
            "SSH-tail directly. The Phase 2 PR relocates that logic here."
        )

    def liveness(self, peer_id: str) -> LivenessResult:
        self._check_alive()
        raise NotImplementedError(
            "SshBearer.liveness is Phase 2 work; status surfaces still read "
            "the heartbeat file directly."
        )

    def close(self) -> None:
        # Idempotent per ABC contract.
        self._closed = True
        self._opened_peer_id = None
        self._peer_meta = {}
