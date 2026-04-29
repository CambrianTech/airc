"""Bearer resolver — the ONLY module that imports concrete bearers.

The resolver maintains an ordered registry of bearer types and, given a
peer's metadata, picks the first bearer that says it can serve. Order is
preference: faster / cheaper bearers come first, more-universal ones
later. Callers receive an opaque Bearer instance and never see which
concrete type it is.

Adding a transport: import its class, add to _REGISTRY at the right
preference position. Done. Removing a transport: delete the import and
the registry entry. Done. No other file moves.

Phase 3b (current state): registry has LocalBearer + SshBearer + GhBearer.

GhBearer is registered LAST deliberately. Real production peer_meta
populates host_target (so SshBearer.can_serve wins), so GhBearer only
activates for peer_meta where host_target is empty but room_gist_id is
set — a path the resolver doesn't reach in production today. This keeps
3b purely additive: SshBearer keeps serving today's traffic; GhBearer
exists in the seam, exercised by tests, ready for Phase 3c to flip.

Phase 3c flips order to [LocalBearer, GhBearer], removes SshBearer +
Tailscale entirely, and updates the join handshake so cross-network
pairings populate room_gist_id (not host_target) — at which point
GhBearer takes over all non-loopback traffic.
"""

from __future__ import annotations

from typing import List, Type

from .bearer import Bearer, PeerUnreachable
from .bearer_local import LocalBearer
from .bearer_gh import GhBearer
from .bearer_ssh import SshBearer

# Preference order. Earlier = preferred. The resolver tries each in turn
# via can_serve() and falls through on PeerUnreachable from open().
#   LocalBearer — same-machine peers skip the SSH layer entirely.
#   SshBearer   — direct-network peers (Tailscale, LAN, public). What
#                 production uses today.
#   GhBearer    — gh-as-bearer fallback for peers without direct-network
#                 reachability. After Phase 3c becomes the cross-network
#                 default and SshBearer is removed.
_REGISTRY: List[Type[Bearer]] = [
    LocalBearer,
    SshBearer,
    GhBearer,
]


def available_kinds() -> List[str]:
    """Names of registered bearer kinds, in preference order. Used by
    `airc doctor` and status surfaces to report what transports the
    binary can speak."""
    return [b.KIND for b in _REGISTRY]


def resolve(peer_meta: dict) -> Bearer:
    """Pick a bearer for the given peer metadata.

    Iterates _REGISTRY in preference order; returns the first bearer
    whose can_serve() returns True. Raises PeerUnreachable if no bearer
    can serve the peer — at that point the peer truly is unreachable
    by any means we know.

    The returned bearer is instantiated but NOT opened. Callers must
    call open(peer_id) before send/recv. This split keeps resolution
    cheap for status surfaces that want to know "what would we use"
    without committing to a connection.
    """
    candidates = [b for b in _REGISTRY if b.can_serve(peer_meta)]
    if not candidates:
        raise PeerUnreachable(
            f"no registered bearer can serve peer_meta={peer_meta!r}; "
            f"available kinds: {available_kinds()}"
        )
    # Use the first candidate. Future enhancement: try each in turn,
    # fall through on PeerUnreachable from open(). That's intentionally
    # not in Phase 1 — it'd require concrete bearers to be cheap to
    # construct, which is the documented invariant but not yet tested.
    return candidates[0](peer_meta)
