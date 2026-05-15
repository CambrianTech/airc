"""collaboration health tests.

Run: cd test && python3 test_collaboration.py
"""

from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stdout, redirect_stderr
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import collaboration  # noqa: E402


class CollaborationHealthTests(unittest.TestCase):
    def _scope(self):
        tmp = tempfile.TemporaryDirectory()
        home = Path(tmp.name)
        (home / "peers").mkdir()
        return tmp, home

    def _remote_line(self, sender="remote-agent", client_id=None):
        msg = {
            "from": sender,
            "to": "all",
            "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "channel": "general",
            "msg": "hello",
        }
        if client_id is not None:
            msg["client_id"] = client_id
        return json.dumps({
            **msg,
        }) + "\n"

    def test_status_waiting_without_records_or_remote_traffic(self):
        tmp, home = self._scope()
        with tmp:
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main(["status", "--home", str(home), "--my-name", "me"])
            self.assertEqual(rc, 0)
            self.assertIn("collaboration: waiting for peers", out.getvalue())

    def test_doctor_info_without_records_or_remote_traffic(self):
        tmp, home = self._scope()
        with tmp:
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main(["doctor", "--home", str(home), "--my-name", "me"])
            self.assertEqual(rc, 0)
            self.assertIn("waiting for first peer", out.getvalue())

    def test_doctor_blocked_when_remote_history_is_stale(self):
        tmp, home = self._scope()
        with tmp:
            stale_ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 7200))
            (home / "messages.jsonl").write_text(json.dumps({
                "from": "remote-agent",
                "to": "all",
                "ts": stale_ts,
                "channel": "general",
                "msg": "old",
            }) + "\n", encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main(["doctor", "--home", str(home), "--my-name", "me"])
            self.assertEqual(rc, 2)
            self.assertIn("may be a solo island", out.getvalue())

    def test_status_ok_when_recent_remote_traffic_exists(self):
        tmp, home = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(self._remote_line(), encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main(["status", "--home", str(home), "--my-name", "me"])
            self.assertEqual(rc, 0)
            text = out.getvalue()
            self.assertIn("collaboration: ok (1 broadcast peer", text)
            self.assertIn("Presence is derived", text)
            self.assertNotIn("collaboration: SOLO", text)

    def test_status_ok_when_same_name_different_client_id_exists(self):
        tmp, home = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(
                self._remote_line(sender="me", client_id="agent:other"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main([
                    "status",
                    "--home", str(home),
                    "--my-name", "me",
                    "--client-id", "agent:self",
                ])
            self.assertEqual(rc, 0)
            text = out.getvalue()
            self.assertIn("collaboration: ok (1 broadcast peer", text)
            self.assertIn("me [agent:other]", text)
            self.assertNotIn("collaboration: SOLO", text)

    def test_status_ignores_same_client_id_as_self(self):
        tmp, home = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(
                self._remote_line(sender="me", client_id="agent:self"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main([
                    "status",
                    "--home", str(home),
                    "--my-name", "me",
                    "--client-id", "agent:self",
                ])
            self.assertEqual(rc, 0)
            self.assertIn("collaboration: waiting for peers", out.getvalue())

    def test_doctor_ok_when_recent_remote_traffic_exists(self):
        tmp, home = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(self._remote_line(), encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main(["doctor", "--home", str(home), "--my-name", "me"])
            self.assertEqual(rc, 0)
            self.assertIn("recent broadcast peer", out.getvalue())

    def test_send_warning_silent_when_remote_traffic_exists(self):
        tmp, home = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(self._remote_line(), encoding="utf-8")
            err = io.StringIO()
            with redirect_stderr(err):
                rc = collaboration.main(["send-warning", "--home", str(home), "--my-name", "me"])
            self.assertEqual(rc, 0)
            self.assertEqual("", err.getvalue())

    def test_peers_fallback_lists_recent_broadcast_speaker(self):
        tmp, home = self._scope()
        with tmp:
            os.rmdir(home / "peers")
            (home / "messages.jsonl").write_text(self._remote_line(), encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main(["peers-fallback", "--home", str(home), "--my-name", "me"])
            self.assertEqual(rc, 0)
            self.assertIn("remote-agent", out.getvalue())
            self.assertIn("broadcast room", out.getvalue())

    def test_peers_fallback_lists_same_name_broadcast_speaker_by_client_id(self):
        tmp, home = self._scope()
        with tmp:
            os.rmdir(home / "peers")
            (home / "messages.jsonl").write_text(
                self._remote_line(sender="me", client_id="agent:other"),
                encoding="utf-8",
            )
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main([
                    "peers-fallback",
                    "--home", str(home),
                    "--my-name", "me",
                    "--client-id", "agent:self",
                ])
            self.assertEqual(rc, 0)
            self.assertIn("me [agent:other]", out.getvalue())
            self.assertIn("broadcast room", out.getvalue())

    def test_whois_fallback_describes_recent_broadcast_speaker(self):
        tmp, home = self._scope()
        with tmp:
            (home / "messages.jsonl").write_text(self._remote_line(), encoding="utf-8")
            out = io.StringIO()
            with redirect_stdout(out):
                rc = collaboration.main([
                    "whois-fallback",
                    "--home", str(home),
                    "--my-name", "me",
                    "--peer-name", "remote-agent",
                ])
            self.assertEqual(rc, 0)
            text = out.getvalue()
            self.assertIn("name:      remote-agent", text)
            self.assertIn("role:      broadcast peer", text)


if __name__ == "__main__":
    unittest.main()
