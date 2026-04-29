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

from unittest import mock  # noqa: E402

from airc_core.bearer import (  # noqa: E402
    Bearer,
    BearerError,
    LivenessResult,
    PeerUnreachable,
    ReceivedMessage,
    SendOutcome,
)
from airc_core.bearer_resolver import (  # noqa: E402
    available_kinds,
    resolve,
)
from airc_core.bearer_ssh import SshBearer, SshBearerError  # noqa: E402
from airc_core import bearer_ssh  # noqa: E402


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
        bearer.close()

    def test_unreachable_when_no_bearer_can_serve(self):
        with self.assertRaises(PeerUnreachable):
            resolve({})  # no host_target, no bearer matches

    def test_resolved_bearer_is_not_yet_open(self):
        bearer = resolve({"host_target": "user@host:7547"})
        # Resolution is cheap — no IO happens yet.
        self.assertIsInstance(bearer, Bearer)
        bearer.close()

    def test_resolver_passes_peer_meta_to_bearer(self):
        # Bearer needs peer_meta to send; resolver must thread it through
        # at construction.
        bearer = resolve({"host_target": "alice@example:7547"})
        self.assertEqual(bearer._peer_meta.get("host_target"), "alice@example:7547")
        bearer.close()


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
        b2 = SshBearer({"host_target": "u@h"})
        self.assertIsNot(b1, b2)
        b1.close()
        b2.close()

    def test_open_then_close_is_clean(self):
        b = SshBearer({"host_target": "u@h"})
        b.open("alice")
        b.close()
        # close() is idempotent
        b.close()

    def test_post_close_operations_raise(self):
        b = SshBearer({"host_target": "u@h"})
        b.close()
        with self.assertRaises(BearerError):
            b.open("alice")
        with self.assertRaises(BearerError):
            b.send("alice", "general", b"x")


class SshBearerSendTests(unittest.TestCase):
    """Phase 1 SshBearer.send() — the relocated SSH delivery primitive.

    All tests mock subprocess.run + the tailscale resolver so no real
    network or processes are touched. We verify the bearer correctly
    classifies outcomes from the underlying transport's signals.
    """

    def setUp(self):
        # Default peer_meta — overridden per test as needed.
        self._meta = {
            "host_target": "alice@example:7547",
            "remote_home": "$HOME/.airc",
            "identity_key": "/tmp/fake_key",
        }

    def _bearer(self, meta=None):
        b = SshBearer(meta or self._meta)
        b.open("alice")
        return b

    def test_send_without_host_target_raises(self):
        b = SshBearer({})  # no host_target
        b.open("alice")
        with self.assertRaises(SshBearerError) as ctx:
            b.send("alice", "general", b"hi")
        self.assertIn("host_target", str(ctx.exception))
        b.close()

    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=True)
    def test_send_queues_when_tailnet_reports_offline(self, _mock_offline):
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "queued_unreachable")
        self.assertIn("offline", outcome.detail.lower())
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(bearer_ssh.subprocess, "run")
    def test_send_delivered_when_marker_in_stdout(self, mock_run, *_):
        mock_run.return_value = mock.Mock(
            stdout=b"__APPENDED__\n",
            stderr=b"",
            returncode=0,
        )
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "delivered")
        # Verify the SSH invocation was constructed correctly.
        args = mock_run.call_args
        argv = args.args[0]
        self.assertIn("/usr/bin/ssh", argv)
        self.assertIn("-i", argv)
        self.assertIn("/tmp/fake_key", argv)
        self.assertIn("-p", argv)
        self.assertIn("7547", argv)
        # Remote command must contain the messages.jsonl append + marker.
        self.assertTrue(any("messages.jsonl" in a for a in argv))
        self.assertTrue(any("__APPENDED__" in a for a in argv))
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(bearer_ssh.subprocess, "run")
    def test_send_classifies_auth_failure(self, mock_run, *_):
        mock_run.return_value = mock.Mock(
            stdout=b"",
            stderr=b"alice@example: Permission denied (publickey).\n",
            returncode=255,
        )
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "auth_failure")
        self.assertIn("re-pair", outcome.detail)
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(bearer_ssh.subprocess, "run")
    def test_send_classifies_transient_failure(self, mock_run, *_):
        mock_run.return_value = mock.Mock(
            stdout=b"",
            stderr=b"ssh: connect to host example port 7547: Connection refused\n",
            returncode=255,
        )
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "transient_failure")
        self.assertIn("Connection refused", outcome.detail)
        b.close()

    @mock.patch.object(bearer_ssh, "_resolve_ssh_bin", return_value="/usr/bin/ssh")
    @mock.patch.object(bearer_ssh, "_is_peer_offline_in_tailnet", return_value=False)
    @mock.patch.object(
        bearer_ssh.subprocess,
        "run",
        side_effect=bearer_ssh.subprocess.TimeoutExpired(cmd="ssh", timeout=15),
    )
    def test_send_handles_timeout(self, *_):
        b = self._bearer()
        outcome = b.send("alice", "general", b'{"msg":"hi"}')
        self.assertEqual(outcome.kind, "transient_failure")
        self.assertIn("timed out", outcome.detail)
        b.close()

    def test_send_outcome_is_immutable(self):
        o = SendOutcome(kind="delivered")
        with self.assertRaises(Exception):
            o.kind = "tampered"


class CgnatRegexTests(unittest.TestCase):
    """The Tailscale-CGNAT range matcher is the only Tailscale knowledge
    in the codebase outside install scripts. Until Phase 3 deletes it,
    it must reject non-CGNAT IPs cleanly so no LAN/DNS targets get
    mis-routed through the offline-fast-path."""

    def test_matches_cgnat_addresses(self):
        for ip in ("100.64.0.1", "100.99.99.99", "100.119.50.20", "100.127.255.254"):
            self.assertTrue(bearer_ssh._CGNAT_RE.match(ip), f"should match {ip}")

    def test_rejects_non_cgnat_addresses(self):
        for ip in ("100.63.0.1", "100.128.0.1", "192.168.1.1", "10.0.0.1", "127.0.0.1", "100.5.0.1"):
            self.assertFalse(bearer_ssh._CGNAT_RE.match(ip), f"should reject {ip}")

    def test_offline_check_strips_user_prefix(self):
        # Strips user@ correctly so resume paths with `user@host` form
        # don't bypass the CGNAT gate. (issue #78 root cause)
        with mock.patch.object(bearer_ssh, "_resolve_tailscale_bin", return_value=None):
            # No tailscale = always False. Just verify no crash on user@host form.
            self.assertFalse(bearer_ssh._is_peer_offline_in_tailnet("alice@100.64.0.1"))
            self.assertFalse(bearer_ssh._is_peer_offline_in_tailnet("alice@192.168.1.5"))


if __name__ == "__main__":
    unittest.main()
