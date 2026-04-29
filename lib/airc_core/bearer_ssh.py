"""SshBearer — message transport over SSH.

ALL SSH-specific knowledge lives in this module: ssh binary location, key
selection, host/port resolution, MSYS path translation, the
`__APPENDED__` confirmation protocol, error classification (auth vs
network), pending-queue semantics for offline peers. Code outside this
file does not mention SSH.

If a future contributor needs to find "how does airc do SSH," the answer
is "open this file." If they need to add a new transport (gh, Reticulum,
LoRa, websocket, anything), they write a sibling file in the same shape
and register it in bearer_resolver.py. They never touch this one.

Phase 0 (current state): skeleton. KIND + can_serve + cheap-construct
invariant satisfied; send/recv/liveness/close raise NotImplementedError
with explicit guidance pointing to the upcoming Phase 1 PR. The skeleton
exists so the resolver and the seam tests are real, not vapor.

Phase 1 (next PR): send() becomes functional by relocating cmd_send.sh's
SSH delivery primitive into this module. The local-mirror, queue-on-fail,
and remote __APPENDED__ confirmation logic moves here. cmd_send.sh shrinks
to "build envelope, sign, hand to bearer."

Phase 2: recv_stream() relocates the monitor's SSH-tail logic. liveness()
relocates the heartbeat read.
"""

from __future__ import annotations

from typing import Iterator

from .bearer import (
    Bearer,
    BearerError,
    LivenessResult,
    PeerUnreachable,
    ReceivedMessage,
)


class SshBearerError(BearerError):
    """SSH-transport-class errors. Distinct subclass so callers can branch
    on transport without importing this module by name (they branch on
    isinstance against BearerError subclasses if needed — but the
    architectural preference is never to branch on transport at all)."""


class SshBearer(Bearer):
    KIND = "ssh"

    @classmethod
    def can_serve(cls, peer_meta: dict) -> bool:
        """Return True if peer_meta describes an SSH-reachable peer.

        SSH reachability requires: a `host_target` field populated by the
        pair-handshake (user@host[:port]) AND a corresponding identity
        key on disk. peer_meta is supplied by the caller, the disk-side
        key check is handled lazily in open() — can_serve() stays pure.
        """
        return bool(peer_meta.get("host_target"))

    def __init__(self) -> None:
        # No IO here. Concrete bearers MUST be cheap to instantiate so
        # the resolver can probe candidacy without committing.
        self._opened_peer_id: str | None = None
        self._closed = False

    def open(self, peer_id: str) -> None:
        if self._closed:
            raise SshBearerError("bearer already closed")
        # Phase 0: track open() for Phase 1 to wire up. No actual SSH
        # work yet — that arrives when send() goes functional.
        self._opened_peer_id = peer_id

    def send(self, peer_id: str, channel: str, payload: bytes) -> None:
        if self._closed:
            raise SshBearerError("bearer already closed")
        raise NotImplementedError(
            "SshBearer.send is Phase 1 work; cmd_send.sh still does SSH "
            "delivery directly. The Phase 1 PR relocates that logic here."
        )

    def recv_stream(self) -> Iterator[ReceivedMessage]:
        if self._closed:
            raise SshBearerError("bearer already closed")
        raise NotImplementedError(
            "SshBearer.recv_stream is Phase 2 work; the monitor still does "
            "SSH-tail directly. The Phase 2 PR relocates that logic here."
        )

    def liveness(self, peer_id: str) -> LivenessResult:
        if self._closed:
            raise SshBearerError("bearer already closed")
        raise NotImplementedError(
            "SshBearer.liveness is Phase 2 work; status surfaces still read "
            "the heartbeat file directly."
        )

    def close(self) -> None:
        # Idempotent per ABC contract.
        self._closed = True
        self._opened_peer_id = None
