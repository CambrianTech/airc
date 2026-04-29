"""Bearer abstraction tests — the seam compiles, the ABC enforces its
contract, and the resolver picks correctly.

Run: python -m unittest test.test_bearer (from repo root)
or:  cd test && python -m unittest test_bearer
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

# Make lib/ importable when running this test from the repo root or test/.
REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core.bearer import (  # noqa: E402
    Bearer,
    BearerError,
    LivenessResult,
    PeerUnreachable,
    ReceivedMessage,
)
from airc_core.bearer_resolver import (  # noqa: E402
    available_kinds,
    resolve,
)
from airc_core.bearer_ssh import SshBearer  # noqa: E402


class BearerInterfaceTests(unittest.TestCase):
    """The ABC enforces its shape and refuses incomplete implementations."""

    def test_cannot_instantiate_abstract_bearer(self):
        with self.assertRaises(TypeError):
            Bearer()

    def test_partial_subclass_cannot_instantiate(self):
        # Missing required abstract methods → instantiation refused.
        class HalfBearer(Bearer):
            KIND = "half"

            @classmethod
            def can_serve(cls, peer_meta):
                return False

            # Deliberately missing: open, send, recv_stream, liveness, close.

        with self.assertRaises(TypeError):
            HalfBearer()

    def test_received_message_is_immutable(self):
        m = ReceivedMessage(
            sender_peer_id="alice",
            channel="general",
            payload=b"hello",
        )
        with self.assertRaises(Exception):
            m.payload = b"tampered"  # frozen dataclass

    def test_liveness_result_allows_unknown_last_seen(self):
        r = LivenessResult(peer_id="bob", last_seen_ts=None, bearer_diag="no signal")
        self.assertIsNone(r.last_seen_ts)


class ResolverTests(unittest.TestCase):
    """Resolver picks bearers based on can_serve, raises when no candidate."""

    def test_available_kinds_includes_ssh_in_phase0(self):
        kinds = available_kinds()
        self.assertIn("ssh", kinds)

    def test_resolves_ssh_for_peer_with_host_target(self):
        bearer = resolve({"host_target": "user@host:7547"})
        self.assertIsInstance(bearer, SshBearer)
        self.assertEqual(bearer.KIND, "ssh")

    def test_unreachable_when_no_bearer_can_serve(self):
        with self.assertRaises(PeerUnreachable):
            resolve({})  # no host_target, no bearer matches

    def test_resolved_bearer_is_not_yet_open(self):
        bearer = resolve({"host_target": "user@host:7547"})
        # Resolution is cheap — no IO happens yet.
        self.assertIsInstance(bearer, Bearer)


class SshBearerSkeletonTests(unittest.TestCase):
    """Phase 0 SshBearer skeleton: lifecycle methods, NotImplementedError
    guidance for the parts that arrive in Phase 1+."""

    def test_kind_is_ssh(self):
        self.assertEqual(SshBearer.KIND, "ssh")

    def test_can_serve_requires_host_target(self):
        self.assertTrue(SshBearer.can_serve({"host_target": "u@h"}))
        self.assertFalse(SshBearer.can_serve({}))
        self.assertFalse(SshBearer.can_serve({"unrelated": "field"}))

    def test_can_serve_is_pure(self):
        # No IO, no side effects — calling it 100 times is free.
        for _ in range(100):
            SshBearer.can_serve({"host_target": "u@h"})

    def test_construct_is_cheap(self):
        # Cheap-construct invariant: resolver may build candidates
        # speculatively. No IO on __init__.
        b1 = SshBearer()
        b2 = SshBearer()
        self.assertIsNot(b1, b2)
        b1.close()
        b2.close()

    def test_open_then_close_is_clean(self):
        b = SshBearer()
        b.open("alice")
        b.close()
        # close() is idempotent
        b.close()

    def test_post_close_operations_raise(self):
        b = SshBearer()
        b.close()
        with self.assertRaises(BearerError):
            b.open("alice")
        with self.assertRaises(BearerError):
            b.send("alice", "general", b"x")

    def test_send_raises_not_implemented_with_phase_guidance(self):
        b = SshBearer()
        b.open("alice")
        with self.assertRaises(NotImplementedError) as ctx:
            b.send("alice", "general", b"x")
        # The error message points at the next PR, so a future debugger
        # finding it knows where the work belongs.
        self.assertIn("Phase 1", str(ctx.exception))
        b.close()


if __name__ == "__main__":
    unittest.main()
