#!/usr/bin/env python3
"""Test airc#607/continuum#1192: monitor metronome fan-out across the
active channel roster.

Two layers of coverage:

1. `test_metronome_all_config_write` — drives the real `airc` binary with
   a temp `AIRC_HOME`, asserts `--all` writes the `*roster*` sentinel and
   that `--all` + `--owner` is rejected up-front.

2. `test_roster_extraction_logic` — mirrors the embedded Python that the
   monitor's reminder_timer_loop runs to build the recipient list from
   messages.jsonl. Drift between this fixture and the real code is the
   bug-shape we're protecting against: if the monitor logic changes its
   filter rules without the test catching it, fan-out either over-pings
   (system noise) or under-pings (idle peers stay idle, regressing 1192).
"""

import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from datetime import datetime, timezone


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    return os.path.dirname(here)


AIRC = os.path.join(_repo_root(), "airc")


class MetronomeAllConfigWrite(unittest.TestCase):
    """Driving the real airc binary — proves the CLI surface."""

    def test_all_writes_roster_sentinel(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = {**os.environ, "AIRC_HOME": tmp, "AIRC_NO_GENERAL": "1"}
            r = subprocess.run(
                [AIRC, "queue", "metronome", "CambrianTech/airc",
                 "--all", "--interval", "60", "--roster-window", "3600"],
                cwd="/tmp", env=env, capture_output=True, text=True,
            )
            self.assertEqual(r.returncode, 0, msg=r.stderr)
            cfg_path = os.path.join(tmp, "queue_metronome")
            self.assertTrue(os.path.exists(cfg_path),
                            f"config not written; stderr={r.stderr!r}")
            with open(cfg_path) as f:
                cfg = dict(line.strip().split("=", 1)
                           for line in f if "=" in line)
            self.assertEqual(cfg.get("owner"), "*roster*")
            self.assertEqual(cfg.get("roster_window"), "3600")
            self.assertEqual(cfg.get("interval"), "60")
            self.assertEqual(cfg.get("repo"), "CambrianTech/airc")

    def test_all_rejects_owner_combo(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = {**os.environ, "AIRC_HOME": tmp, "AIRC_NO_GENERAL": "1"}
            r = subprocess.run(
                [AIRC, "queue", "metronome", "CambrianTech/airc",
                 "--all", "--owner", "joel"],
                cwd="/tmp", env=env, capture_output=True, text=True,
            )
            self.assertNotEqual(r.returncode, 0,
                                "expected refusal when --all + --owner combined")
            self.assertIn("mutually exclusive", r.stderr.lower(),
                          f"expected mutex error; got: {r.stderr!r}")

    def test_roster_window_validation(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = {**os.environ, "AIRC_HOME": tmp, "AIRC_NO_GENERAL": "1"}
            r = subprocess.run(
                [AIRC, "queue", "metronome", "CambrianTech/airc",
                 "--all", "--roster-window", "10"],
                cwd="/tmp", env=env, capture_output=True, text=True,
            )
            self.assertNotEqual(r.returncode, 0,
                                "expected refusal for sub-minimum window")
            self.assertIn("roster-window", r.stderr.lower())


# Roster extraction logic copied verbatim from `airc` monitor loop
# (reminder_timer_loop). Keep in sync — divergence is the bug surface.
def _extract_roster(messages_path, window_s, me):
    now = time.time()
    seen = {}
    if os.path.exists(messages_path):
        with open(messages_path) as f:
            for line in f:
                try:
                    m = json.loads(line)
                except Exception:
                    continue
                who = m.get("from") or ""
                if not who or who == "airc" or who == me:
                    continue
                ts = m.get("ts")
                try:
                    if isinstance(ts, (int, float)):
                        t = float(ts)
                        if t > 1e12:
                            t = t / 1000.0
                    else:
                        t = datetime.fromisoformat(
                            str(ts).replace("Z", "+00:00")
                        ).timestamp()
                except Exception:
                    continue
                if now - t > window_s:
                    continue
                if t > seen.get(who, 0):
                    seen[who] = t
    return [who for who in sorted(seen, key=lambda k: seen[k], reverse=True)]


class RosterExtraction(unittest.TestCase):
    """Unit tests for the python embedded in reminder_timer_loop."""

    def _write_fixture(self, path, lines):
        with open(path, "w") as f:
            for line in lines:
                f.write(json.dumps(line) + "\n")

    def test_recent_senders_listed_newest_first(self):
        now = time.time()
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            self._write_fixture(log, [
                {"from": "alice", "ts": now - 60, "to": "all", "msg": "hi"},
                {"from": "bob",   "ts": now - 30, "to": "all", "msg": "hey"},
                {"from": "carol", "ts": now - 5,  "to": "all", "msg": "yo"},
            ])
            roster = _extract_roster(log, window_s=3600, me="")
            self.assertEqual(roster, ["carol", "bob", "alice"])

    def test_filters_self_and_airc_system_sender(self):
        now = time.time()
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            self._write_fixture(log, [
                {"from": "airc",       "ts": now - 10, "to": "all", "msg": "boot"},
                {"from": "claude-tab", "ts": now - 5,  "to": "all", "msg": "yo"},
                {"from": "alice",      "ts": now - 20, "to": "all", "msg": "hi"},
            ])
            roster = _extract_roster(log, window_s=3600, me="claude-tab")
            self.assertEqual(roster, ["alice"])

    def test_drops_anyone_outside_window(self):
        now = time.time()
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            self._write_fixture(log, [
                {"from": "ancient", "ts": now - 90000, "to": "all", "msg": "old"},
                {"from": "fresh",   "ts": now - 30,    "to": "all", "msg": "new"},
            ])
            roster = _extract_roster(log, window_s=86400, me="")
            self.assertEqual(roster, ["fresh"])

    def test_handles_iso_string_timestamps(self):
        now_iso = datetime.now(timezone.utc).isoformat()
        old_iso = "2020-01-01T00:00:00Z"
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            self._write_fixture(log, [
                {"from": "alice", "ts": now_iso, "to": "all", "msg": "hi"},
                {"from": "old",   "ts": old_iso, "to": "all", "msg": "ancient"},
            ])
            roster = _extract_roster(log, window_s=3600, me="")
            self.assertEqual(roster, ["alice"])

    def test_handles_epoch_milliseconds(self):
        now_ms = int(time.time() * 1000)
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            self._write_fixture(log, [
                {"from": "alice", "ts": now_ms, "to": "all", "msg": "hi"},
            ])
            roster = _extract_roster(log, window_s=3600, me="")
            self.assertEqual(roster, ["alice"])

    def test_dedupes_by_latest_timestamp(self):
        now = time.time()
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            self._write_fixture(log, [
                {"from": "alice", "ts": now - 600, "to": "all", "msg": "old hi"},
                {"from": "alice", "ts": now - 5,   "to": "all", "msg": "recent hi"},
                {"from": "alice", "ts": now - 300, "to": "all", "msg": "middle"},
            ])
            roster = _extract_roster(log, window_s=3600, me="")
            self.assertEqual(roster, ["alice"])  # one entry, not three

    def test_empty_log_returns_empty_roster(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            open(log, "w").close()
            self.assertEqual(_extract_roster(log, window_s=3600, me=""), [])

    def test_missing_log_returns_empty_roster(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")  # never created
            self.assertEqual(_extract_roster(log, window_s=3600, me=""), [])

    def test_malformed_lines_are_skipped_not_fatal(self):
        now = time.time()
        with tempfile.TemporaryDirectory() as tmp:
            log = os.path.join(tmp, "messages.jsonl")
            with open(log, "w") as f:
                f.write("not json\n")
                f.write(json.dumps({"from": "alice", "ts": now - 5}) + "\n")
                f.write("{broken\n")
            self.assertEqual(_extract_roster(log, window_s=3600, me=""),
                             ["alice"])


if __name__ == "__main__":
    if not os.path.exists(AIRC) or not os.access(AIRC, os.X_OK):
        print(f"FATAL: {AIRC} not executable", file=sys.stderr)
        sys.exit(2)
    unittest.main(verbosity=2)
