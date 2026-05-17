"""Heartbeat emission integration tests (airc#644 PR-2).

PR-1 (#645) landed the data model + display logic. PR-2 wires the
actual emission: cmd_send --heartbeat flag, reminder_timer_loop hook,
monitor_formatter UI filter. These tests validate the end-to-end:

1. cmd_send --heartbeat stamps kind=heartbeat on the envelope.
2. monitor_formatter filters kind=heartbeat out of UI rendering AND
   arms the watchdog (any inbound traffic = bearer is alive).
3. The reminder_timer_loop hook is shaped to fire every 60s.

Live cross-peer emission requires a multi-peer test harness which is
out of scope here; the integration suite (test/integration.sh) covers
that. These tests are unit/structural — they catch regressions in the
field shape without needing two airc instances running.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import heartbeat


class CmdSendHeartbeatFlagShapeTests(unittest.TestCase):
    """The --heartbeat flag in cmd_send.sh: we don't shell-out here
    (cross-platform CI fragility) but we verify the constants the bash
    side reads match the Python side.

    The bash-side payload stamp is:
        [ "$heartbeat" = "1" ] && payload="${payload},\"kind\":\"heartbeat\""

    The kind literal must match `heartbeat.HEARTBEAT_KIND`.
    """

    def test_kind_literal_matches_python_constant(self):
        # If someone renames HEARTBEAT_KIND in Python without updating the
        # bash literal, cmd_peers wouldn't classify the envelope correctly.
        # This test pins the literal so a rename forces a coordinated update.
        self.assertEqual(heartbeat.HEARTBEAT_KIND, "heartbeat")

    def test_cadence_matches_bash_default(self):
        # cmd_send-side: local heartbeat_cadence=60 in reminder_timer_loop.
        # python-side: heartbeat.HEARTBEAT_CADENCE_SEC = 60.
        # These must agree — if cadence diverges, STALE_HEARTBEAT_SEC's 2x
        # gate produces false PROCESS_DOWN signals during the divergence
        # window. Drift here is invisible without this test.
        self.assertEqual(heartbeat.HEARTBEAT_CADENCE_SEC, 60)


class MonitorFormatterHeartbeatFilterTests(unittest.TestCase):
    """The monitor_formatter filter (airc#644 PR-2): heartbeats are
    protocol traffic and must never appear in user-visible output.
    They DO arm the inbound-watchdog (any inbound traffic proves the
    bearer is alive)."""

    def test_filter_recognizes_heartbeat(self):
        """The predicate any filter path consults."""
        env = {
            "from": "alice",
            "to": "all",
            "ts": "2026-05-17T01:00:00Z",
            "channel": "general",
            "kind": "heartbeat",
            "msg": "",
        }
        self.assertTrue(heartbeat.is_heartbeat(env))

    def test_filter_does_not_match_chat(self):
        env = {"from": "alice", "msg": "hello", "kind": "chat"}
        self.assertFalse(heartbeat.is_heartbeat(env))

    def test_filter_does_not_match_legacy_envelope(self):
        """Pre-#644 peers don't emit kind. Their chat must NOT be
        treated as heartbeat (would silently drop legit chat)."""
        env = {"from": "alice", "msg": "legacy chat message"}
        self.assertFalse(heartbeat.is_heartbeat(env))

    def test_filter_imports_cleanly(self):
        """The monitor_formatter imports airc_core.heartbeat at module
        load. If that import fails, the formatter crashes on first
        message. Verify the symbol surface is what the formatter expects.
        """
        # Symbols the formatter uses:
        self.assertTrue(callable(heartbeat.is_heartbeat))
        # Note: monitor_formatter today inlines the kind comparison
        # rather than calling is_heartbeat — for cheaper hot-path
        # performance. The constant HEARTBEAT_KIND is the contract:
        self.assertEqual(heartbeat.HEARTBEAT_KIND, "heartbeat")


class ReminderTimerLoopShapeTests(unittest.TestCase):
    """Structural tests for the bash reminder_timer_loop hook. We don't
    boot the bash loop (would require fork + signal handling); we verify
    that the literal cadence in the bash file matches the Python constant
    so drift is caught at test-time, not in production."""

    def test_bash_heartbeat_cadence_matches_python(self):
        """Grep the airc bash script for the heartbeat_cadence literal
        and confirm it equals HEARTBEAT_CADENCE_SEC."""
        airc_bash = REPO_ROOT / "airc"
        content = airc_bash.read_text()
        # Find the literal: "local heartbeat_cadence=60"
        import re
        m = re.search(r"local heartbeat_cadence=(\d+)", content)
        self.assertIsNotNone(m, "heartbeat_cadence literal not found in airc bash script")
        bash_value = int(m.group(1))
        self.assertEqual(
            bash_value, heartbeat.HEARTBEAT_CADENCE_SEC,
            f"bash heartbeat_cadence={bash_value} disagrees with "
            f"Python HEARTBEAT_CADENCE_SEC={heartbeat.HEARTBEAT_CADENCE_SEC}. "
            f"Drift here produces false PROCESS_DOWN signals.",
        )


class CmdSendHeartbeatFlagWiringTests(unittest.TestCase):
    """Verify the --heartbeat flag is wired into cmd_send.sh's flag
    parser. Structural grep — catches bit-rot if someone refactors flag
    handling without preserving the --heartbeat case."""

    def test_flag_parser_recognizes_heartbeat(self):
        cmd_send = REPO_ROOT / "lib" / "airc_bash" / "cmd_send.sh"
        content = cmd_send.read_text()
        self.assertIn("--heartbeat)", content,
                      "--heartbeat case missing from cmd_send.sh flag parser")
        self.assertIn("heartbeat=1", content,
                      "heartbeat=1 assignment missing from --heartbeat case")

    def test_payload_includes_kind_when_heartbeat_set(self):
        cmd_send = REPO_ROOT / "lib" / "airc_bash" / "cmd_send.sh"
        content = cmd_send.read_text()
        # The line that conditionally appends ,"kind":"heartbeat" must
        # exist. The exact form may vary as long as the literal
        # "kind":"heartbeat" appears AND it's gated on the heartbeat flag.
        self.assertIn('"kind\\":\\"heartbeat\\"', content,
                      "kind=heartbeat stamp missing from cmd_send.sh payload construction")


if __name__ == "__main__":
    unittest.main()
