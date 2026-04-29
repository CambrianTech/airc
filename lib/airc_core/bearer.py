"""Bearer abstraction — the seam between airc and any message transport.

A bearer carries opaque payload bytes between identified peers across some
transport (today: SSH; coming: gh; future: anything). The interface speaks
ONLY in peer-id strings, channel names, opaque bytes, and lifecycle. It
does not know about SSH, gh, addresses, ports, hosts, signatures, or
identities.

If a future reader can read this file alone and tell which transport airc
uses, the abstraction is wrong. The abstraction is the seam; the seam's
job is to be ignorant of what's on either side.

Modularity invariant (load-bearing — do not weaken):
    Concrete bearers (bearer_ssh.py, bearer_gh.py, etc.) are imported by
    bearer_resolver.py and NOWHERE ELSE. Caller code (cmd_send, monitor)
    obtains a Bearer instance via the resolver and only ever sees the
    abstract interface. Adding a transport = one new file + one resolver
    case. Removing a transport = delete one file + delete one resolver
    case. If anything else has to move, the seam leaked.

Per-transport encapsulation (the adapter rule):
    Each transport's knowledge lives ENTIRELY in its bearer module.
    Tailscale-specific code (address scopes, daemon checks, sign-in
    nudges, share-node prompts, anything that mentions Tailscale by
    name) lives only in bearer_tailscale.py — not in install scripts,
    not in cmd_connect, not in the address picker. gh-as-transport
    code (gist mutation, ETag handling, polling cadence, gh-API rate
    limit handling) lives only in bearer_gh.py.

    Note the role qualifier on gh: gh ALSO serves as airc's identity /
    room registry / control plane (gist-as-registry, gh-account as
    namespace). That is a separate concern from gh-as-transport, and
    its code lives elsewhere (cmd_rooms, gistparse). The bearer
    abstraction encapsulates gh ONLY in its transport role. If a
    bearer module ends up doing identity or room-registry work, the
    seam leaked the wrong way.

    Test of correctness: grep for the transport's name (e.g. "tailscale",
    "ssh", "ssh-tail") across the codebase. After Phase 3, every match
    that survives is in that transport's bearer module or its tests.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Iterator, Optional


@dataclass(frozen=True)
class ReceivedMessage:
    """A message handed up by a bearer.

    `payload` is opaque bytes. The bearer does not parse it — signing,
    envelope format, and identity verification are caller concerns
    layered on top. `bearer_metadata` is a free-form diagnostic dict
    populated by the bearer (e.g. SSH latency, gh ETag, gist file shard
    name); callers must not introspect it for routing decisions.
    """
    sender_peer_id: str
    channel: str
    payload: bytes
    bearer_metadata: dict = field(default_factory=dict)


@dataclass(frozen=True)
class SendOutcome:
    """Structured result of a send() call.

    `kind` is one of a small enumerated set, deliberately bearer-agnostic
    so callers can branch on the outcome without knowing which transport
    produced it:

      "delivered"          — bytes accepted by the destination.
      "queued_unreachable" — peer known-offline pre-attempt; payload queued
                             locally for automatic retry. Not a failure;
                             the bearer chose this path to avoid wasting
                             a 10s connect timeout on a predictable miss.
      "auth_failure"       — destination refused our identity. Retry is
                             futile; the user must re-pair. Caller should
                             surface this loudly.
      "transient_failure"  — destination unreachable for a probably-transient
                             reason (network blip, peer just bouncing).
                             Caller should queue + retry.

    `detail` is a short human-readable string for surfacing in user-facing
    output ([QUEUED] markers, error messages, status surfaces). Bearers
    populate it with whatever the transport told them; callers do not
    parse it.

    Bearers MUST NOT invent kinds outside this set without first widening
    the bearer.py contract. Adding a kind without updating callers means
    new outcomes silently get the wrong handling.
    """
    kind: str
    detail: str = ""


@dataclass(frozen=True)
class LivenessResult:
    """Result of a liveness probe against a peer.

    `last_seen_ts` is a Unix timestamp of the most recent activity the
    bearer can attest to, or None if the bearer has no signal. `bearer_diag`
    is a short human-readable diagnostic string for surfacing in `airc
    status` / `airc doctor`.
    """
    peer_id: str
    last_seen_ts: Optional[float]
    bearer_diag: str


class BearerError(Exception):
    """Base class for bearer-layer errors. Concrete bearers raise
    subclasses (e.g. SshBearerError, GhBearerError) so callers can branch
    on transport-class without importing concrete bearer modules."""


class PeerUnreachable(BearerError):
    """Raised by open() when a peer cannot be reached via this bearer.
    Callers (typically the resolver) should fall through to a different
    bearer rather than treating this as fatal."""


class Bearer(ABC):
    """Abstract bearer interface.

    Lifecycle: open() → send()/recv_stream()/liveness() (any order, any
    number of times) → close(). open() may be called multiple times for
    different peer_ids on the same bearer instance — bearers are
    multi-peer where transport allows.

    Implementations MUST be safe to instantiate without side effects;
    network/IO happens in open(). This keeps the resolver cheap and lets
    `airc doctor` introspect available bearers without committing to
    connections.

    Each concrete bearer declares a class-level `KIND` string (its short
    name, e.g. "ssh", "gh", "tailscale", "local") and a class method
    `can_serve(peer_meta)` that returns whether it can serve a peer given
    that peer's known metadata. The resolver iterates registered bearers
    in preference order, picks the first that says yes, falls through on
    PeerUnreachable. This is the property that makes adding a transport
    a one-file change AND makes runtime fallback (e.g. gh rate-limited
    → try the next bearer) work without callers knowing.
    """

    # Override in concrete subclasses. Resolver and `airc doctor` use this
    # for reporting; never gate behavior on KIND from caller code (that
    # would defeat the abstraction).
    KIND: str = "abstract"

    @classmethod
    @abstractmethod
    def can_serve(cls, peer_meta: dict) -> bool:
        """Return True iff this bearer can plausibly serve a peer with
        the given metadata. Pure inspection — no IO, no side effects.

        Resolver calls this BEFORE instantiating the bearer to decide
        candidacy. A True return is not a delivery guarantee; open() may
        still raise PeerUnreachable when actual connection is attempted.
        """

    @abstractmethod
    def open(self, peer_id: str) -> None:
        """Establish whatever the bearer needs to send/recv to/from peer_id.

        Raises PeerUnreachable if the bearer cannot serve this peer (e.g.
        SshBearer asked about a peer with no reachable IP). Callers should
        treat PeerUnreachable as fall-through-to-next-bearer, not fatal.
        """

    @abstractmethod
    def send(self, peer_id: str, channel: str, payload: bytes) -> SendOutcome:
        """Deliver `payload` to `peer_id` on `channel`. Bytes are opaque.

        Returns a SendOutcome describing what happened. Normal failure
        modes (peer offline, transient network failure, auth refused)
        are reported via the outcome's `kind` field, NOT exceptions.
        Exceptions are reserved for programming errors (calling send on
        a closed bearer, etc.).

        The bearer must populate the outcome promptly — synchronous
        bearers (SSH) before returning, polling bearers (gh) once the
        first write attempt resolves. Long-running retry/backoff is
        the caller's responsibility, not the bearer's.
        """

    @abstractmethod
    def recv_stream(self) -> Iterator[ReceivedMessage]:
        """Yield ReceivedMessage events as they arrive on this bearer.

        Generator; blocks between events. Closing the bearer (close())
        raises StopIteration in the active iteration. Implementations
        must use `--line-buffered`-equivalent IO so events surface
        promptly (matters for low-latency bearers; harmless for polling
        ones).
        """

    @abstractmethod
    def liveness(self, peer_id: str) -> LivenessResult:
        """Probe `peer_id` for liveness via this bearer's natural signal.

        Always returns a LivenessResult — never raises for unreachability.
        last_seen_ts=None means "no signal," not "definitely dead." A bearer
        should distinguish "I have no record of this peer" from "I have a
        stale record" via bearer_diag.
        """

    @abstractmethod
    def close(self) -> None:
        """Tear down all resources held by this bearer.

        Idempotent — calling close() on an already-closed bearer is a no-op.
        After close(), subsequent send()/recv_stream()/liveness() calls
        raise BearerError.
        """
