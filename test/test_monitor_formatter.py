"""monitor_formatter tests — auto-pong handler, heartbeat suppression.

Run: cd test && python3 test_monitor_formatter.py
"""

from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import monitor_formatter as mf  # noqa: E402


class AutoPongTests(unittest.TestCase):
    """Pre-fix #308: auto-pong subprocess never fired even though the
    [PING:uuid] line reached monitor_formatter via the gist. Test pipes
    a [PING:] envelope to run() and asserts subprocess.Popen was called
    with the right argv."""

    def setUp(self):
        self._scope = tempfile.mkdtemp(prefix="airc-mf-test-")
        self._peers = os.path.join(self._scope, "peers")
        os.makedirs(self._peers, exist_ok=True)
        cfg = {
            "name": "alice",
            "subscribed_channels": ["general"],
            "channel_gists": {"general": "abc123"},
        }
        with open(os.path.join(self._scope, "config.json"), "w") as f:
            json.dump(cfg, f)

    def tearDown(self):
        import shutil
        shutil.rmtree(self._scope, ignore_errors=True)

    def _run_with_stdin(self, lines):
        """Pipe `lines` to monitor_formatter.run, capture Popen calls.

        Each line should be a dict (will be json.dumps'd) or a string."""
        body = "\n".join(
            json.dumps(l) if isinstance(l, dict) else str(l)
            for l in lines
        ) + "\n"
        captured_popen = []

        class _FakePopen:
            def __init__(self, argv, **kwargs):
                captured_popen.append(argv)
            def wait(self, *_a, **_k): return 0

        # Capture stdout to keep test output clean.
        with mock.patch.object(mf.sys, "stdin", io.StringIO(body)), \
             mock.patch.object(mf.sys, "stdout", io.StringIO()), \
             mock.patch("subprocess.Popen", _FakePopen):
            mf.run("alice", self._peers)

        return captured_popen

    def test_ping_addressed_to_me_fires_auto_pong(self):
        envelope = {
            "from": "bob",
            "to": "alice",
            "ts": "2026-04-29T00:00:00Z",
            "channel": "general",
            "msg": "[PING:11111111-2222-3333-4444-555555555555]",
        }
        popens = self._run_with_stdin([envelope])
        self.assertEqual(len(popens), 1, f"expected exactly one Popen, got {popens}")
        argv = popens[0]
        self.assertEqual(argv[:2], ["airc", "send"])
        self.assertIn("@bob", argv)
        # PONG with the same uuid, in the same channel as the ping.
        self.assertIn("[PONG:11111111-2222-3333-4444-555555555555]", argv)
        self.assertIn("--channel", argv)
        self.assertIn("general", argv)
        # Plaintext is required for the round-trip — encryption of
        # ping/pong was the actual #308 cause (pair-handshake asymmetry
        # → one side dropped on decrypt → silent timeout).
        self.assertIn("--plaintext", argv,
                      "auto-pong must use --plaintext to dodge pair-handshake asymmetry")

    def test_ping_addressed_to_someone_else_does_not_fire_pong(self):
        envelope = {
            "from": "bob",
            "to": "carol",  # not me
            "ts": "2026-04-29T00:00:00Z",
            "channel": "general",
            "msg": "[PING:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee]",
        }
        popens = self._run_with_stdin([envelope])
        self.assertEqual(popens, [], "must not auto-pong pings addressed to others")

    def test_pong_is_suppressed_not_repeated(self):
        # When an inbound PONG arrives (from another peer's auto-pong),
        # monitor_formatter must NOT fire another PONG. Only the cmd_ping
        # process polls the local log for the PONG marker.
        envelope = {
            "from": "bob",
            "to": "alice",
            "ts": "2026-04-29T00:00:00Z",
            "channel": "general",
            "msg": "[PONG:11111111-2222-3333-4444-555555555555]",
        }
        popens = self._run_with_stdin([envelope])
        self.assertEqual(popens, [], "PONG must not trigger another Popen")

    def test_broadcast_ping_does_not_fire_pong(self):
        # A `to=all` ping is a discovery message the operator reads, not
        # a round-trip. Auto-ponging it floods the room with N pongs.
        envelope = {
            "from": "bob",
            "to": "all",
            "ts": "2026-04-29T00:00:00Z",
            "channel": "general",
            "msg": "[PING:11111111-2222-3333-4444-555555555555]",
        }
        popens = self._run_with_stdin([envelope])
        self.assertEqual(popens, [], "broadcast ping must not auto-pong")


class HeartbeatSuppressionTests(unittest.TestCase):
    """bearer_cli heartbeat lines must be recognized + suppressed +
    arm the watchdog. Display would clutter chat with airc_heartbeat
    JSON every 30s otherwise."""

    def setUp(self):
        self._scope = tempfile.mkdtemp(prefix="airc-mf-hb-test-")
        self._peers = os.path.join(self._scope, "peers")
        os.makedirs(self._peers, exist_ok=True)
        with open(os.path.join(self._scope, "config.json"), "w") as f:
            json.dump({"name": "alice"}, f)

    def tearDown(self):
        import shutil
        shutil.rmtree(self._scope, ignore_errors=True)

    def test_heartbeat_line_is_swallowed_no_stdout_output(self):
        body = json.dumps({"airc_heartbeat": 1, "ts": 0, "channel": "general"}) + "\n"
        out = io.StringIO()
        with mock.patch.object(mf.sys, "stdin", io.StringIO(body)), \
             mock.patch.object(mf.sys, "stdout", out):
            mf.run("alice", self._peers)
        self.assertEqual(out.getvalue(), "", "heartbeat must produce zero stdout output")


if __name__ == "__main__":
    unittest.main()
