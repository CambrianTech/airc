"""Bearer resolver — the ONLY module that imports concrete bearers.

The resolver maintains an ordered registry of bearer types and, given a
peer's metadata, picks the first bearer that says it can serve. Order is
preference: faster / cheaper bearers come first, more-universal ones
later. Callers receive an opaque Bearer instance and never see which
concrete type it is.

Adding a transport: import its class, add to _REGISTRY at the right
preference position. Done. Removing a transport: delete the import and
the registry entry. Done. No other file moves.

Phase 3c+ (current state): registry is [GhBearer] only. Encryption is
handled at the envelope layer (lib/airc_core/envelope.py), making
transport-layer encryption (SSH, WireGuard) redundant and letting us
drop the install-time complexity that came with both — no more
Windows OpenSSH-Server admin elevation, no more Tailscale daemon.

LocalBearer was disabled here on 2026-04-29 after it was caught
silently dropping every joiner-side broadcast. Its premise — "skip the
network roundtrip when the peer is on this filesystem" — was correct
in the SSH-tail era when the host's `messages.jsonl` was the substrate
that joiners tailed. Post-3c the substrate is the room gist; everyone
(host AND joiner, same-machine or not) polls the gist via bearer_cli
recv. LocalBearer's send appended to `<host>/messages.jsonl` directly,
which nobody reads anymore — `bearer.send()` returned `delivered`
(file write succeeded) and the message was eaten. Bug was invisible
because the success contract was at the wrong layer. Until LocalBearer
is rewritten to write to the gist (or to ALSO write to the gist), it
must not be in the registry: the gist is the only correct
destination.

Latency cost: 1-2s typical message latency (gh poll cadence) instead of
SSH's <100ms. For chat-pace AI traffic this is invisible; if a future
use case needs sub-second, a new bearer can be added without touching
this seam.
"""

from __future__ import annotations

from typing import List, Type

from .bearer import Bearer, PeerUnreachable
from .bearer_gh import GhBearer

# Preference order. Earlier = preferred. The resolver tries each in turn
# via can_serve() and falls through on PeerUnreachable from open().
#   GhBearer — gh-gist as transport. Encryption lives at the envelope
#              layer above. Single source of truth post-3c+ (see the
#              module docstring for the LocalBearer history).
_REGISTRY: List[Type[Bearer]] = [
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
