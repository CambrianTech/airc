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


class HostModeWatchdogTests(unittest.TestCase):
    """#383: the no-inbound watchdog must be disabled in host mode.

    Without this gate, a daemon-launched `airc connect` in $HOME/.airc
    (no host_target = host mode) trips the 150s watchdog every quiet
    interval, launchctl re-spawns, and the daemon thrashes with
    last_exit_code=1 while never actually serving messages.
    """

    def setUp(self):
        self._scope = tempfile.mkdtemp(prefix="airc-mf-wd-test-")
        self._peers = os.path.join(self._scope, "peers")
        os.makedirs(self._peers, exist_ok=True)
        # Re-arm the module-level flag in case a prior test disabled it.
        mf._watchdog_active = True

    def tearDown(self):
        import shutil
        shutil.rmtree(self._scope, ignore_errors=True)
        mf._watchdog_active = True

    def _write_config(self, host_target):
        cfg = {"name": "alice"}
        if host_target:
            cfg["host_target"] = host_target
        with open(os.path.join(self._scope, "config.json"), "w") as f:
            json.dump(cfg, f)

    def _run_empty_stdin(self):
        with mock.patch.object(mf.sys, "stdin", io.StringIO("")), \
             mock.patch.object(mf.sys, "stdout", io.StringIO()):
            mf.run("alice", self._peers)

    def test_host_mode_disables_watchdog(self):
        # Host mode = config has no host_target.
        self._write_config(host_target=None)
        with mock.patch.object(mf, "_disable_watchdog", wraps=mf._disable_watchdog) as spy:
            self._run_empty_stdin()
            self.assertEqual(spy.call_count, 1,
                             "host mode must call _disable_watchdog exactly once")
        self.assertFalse(mf._watchdog_active,
                         "watchdog must be inactive after host-mode run()")

    def test_joiner_mode_keeps_watchdog_armed(self):
        # Joiner mode = config carries a non-empty host_target.
        self._write_config(host_target="user@10.0.0.5")
        with mock.patch.object(mf, "_disable_watchdog", wraps=mf._disable_watchdog) as spy:
            self._run_empty_stdin()
            self.assertEqual(spy.call_count, 0,
                             "joiner mode must not call _disable_watchdog")
        self.assertTrue(mf._watchdog_active,
                        "watchdog must remain active after joiner-mode run()")

    def test_missing_config_treated_as_host_mode(self):
        # No config.json at all (transient startup window before cmd_join
        # writes one) — fall through to host mode (is_joiner=False), which
        # disables the watchdog. Conservative: a missing config is more
        # often a fresh host than a joiner with corrupted state, and
        # disabling the watchdog only loses an early-warning probe; real
        # bearer death is still caught by the bash retry loop.
        # (No _write_config call.)
        with mock.patch.object(mf, "_disable_watchdog", wraps=mf._disable_watchdog) as spy:
            self._run_empty_stdin()
            self.assertEqual(spy.call_count, 1,
                             "missing config must default to host-mode behavior")


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


class DisplayFilterLoudDropTests(unittest.TestCase):
    """#399 follow-up to #401: when monitor_formatter's display filter
    drops a peer broadcast (channel name truly differs, e.g.
    'cambriantech' vs subs=['general'] — #401's '#'-prefix tolerance
    cannot help), emit a stdout warning so Claude Code's Monitor wakes
    + the operator sees they need `airc subscribe <channel>`.

    Pre-fix: silent drop produced #399's 9-hour blackout pattern even
    after #401 merged.
    """

    def setUp(self):
        self._scope = tempfile.mkdtemp(prefix="airc-mf-drop-test-")
        self._peers = os.path.join(self._scope, "peers")
        os.makedirs(self._peers, exist_ok=True)
        cfg = {"name": "alice", "subscribed_channels": ["general"]}
        with open(os.path.join(self._scope, "config.json"), "w") as f:
            json.dump(cfg, f)
        # Force warning interval to 0 so a single drop fires the warning
        # immediately — keeps the test deterministic.
        self._saved_interval = mf.DROP_WARN_INTERVAL_SEC
        mf.DROP_WARN_INTERVAL_SEC = 0
        mf._filter_drop_count.clear()
        mf._last_drop_warn_ts = 0.0

    def tearDown(self):
        import shutil
        shutil.rmtree(self._scope, ignore_errors=True)
        mf.DROP_WARN_INTERVAL_SEC = self._saved_interval
        mf._filter_drop_count.clear()
        mf._last_drop_warn_ts = 0.0

    def _run(self, lines):
        body = "\n".join(json.dumps(l) for l in lines) + "\n"
        out = io.StringIO()
        err = io.StringIO()
        with mock.patch.object(mf.sys, "stdin", io.StringIO(body)), \
             mock.patch.object(mf.sys, "stdout", out), \
             mock.patch.object(mf.sys, "stderr", err):
            mf.run("alice", self._peers)
        return out.getvalue(), err.getvalue()

    def test_cross_channel_drop_emits_stdout_warning(self):
        msg = {"from": "bob", "to": "all", "channel": "cambriantech",
               "msg": "should drop", "ts": "2026-05-02T15:00:00Z"}
        out, err = self._run([msg])
        self.assertNotIn("should drop", out,
            "cross-channel msg body must not display when subs filter rejects")
        self.assertIn("WARN display-filtered", out,
            "cross-channel drop must surface to stdout so Monitor wakes")
        self.assertIn("cambriantech", out,
            "warning must name the dropped channel so operator can subscribe")
        self.assertIn("display-filter drop", err,
            "stderr trace must record evidence for daemon.log debugging")

    def test_subscribed_channel_does_not_drop(self):
        msg = {"from": "bob", "to": "all", "channel": "general",
               "msg": "should display", "ts": "2026-05-02T15:00:00Z"}
        out, err = self._run([msg])
        self.assertIn("should display", out,
            "subscribed-channel msg must display normally")
        self.assertNotIn("WARN display-filtered", out,
            "subscribed-channel msg must not trigger drop warning")

    def test_addressed_to_me_bypasses_filter(self):
        msg = {"from": "bob", "to": "alice", "channel": "cambriantech",
               "msg": "DM bypasses filter", "ts": "2026-05-02T15:00:00Z"}
        out, err = self._run([msg])
        self.assertIn("DM bypasses filter", out,
            "DM addressed to us must surface across channel boundary")
        self.assertNotIn("WARN display-filtered", out,
            "DM bypass path must not warn-spam (no actual drop happened)")


if __name__ == "__main__":
    unittest.main()
