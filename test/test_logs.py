"""Tests for machine-readable `airc logs` rendering."""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch


REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import logs  # noqa: E402


class LogsRenderTests(unittest.TestCase):
    def line(self, **fields: object) -> str:
        return json.dumps(fields) + "\n"

    def test_human_render_preserves_existing_shape(self) -> None:
        events = logs.events_from_lines([
            self.line(ts="2026-05-16T01:00:00Z", **{"from": "agent"}, msg="ready")
        ])

        self.assertEqual(logs.render_human(events), "[2026-05-16T01:00:00Z] agent: ready\n")

    def test_json_render_exposes_stable_event_fields(self) -> None:
        events = logs.events_from_lines([
            self.line(
                sig="sig-1",
                ts="2026-05-16T01:00:00Z",
                **{"from": "agent", "to": "all"},
                channel="general",
                msg="ready",
                client_id="client-a",
            )
        ])

        payload = json.loads(logs.render_json(events, since_arg="", count=20))

        self.assertEqual(payload["count"], 20)
        self.assertEqual(payload["events"][0]["id"], "sig-1")
        self.assertEqual(payload["events"][0]["sender"], "agent")
        self.assertEqual(payload["events"][0]["recipient"], "all")
        self.assertEqual(payload["events"][0]["channel"], "general")
        self.assertEqual(payload["events"][0]["client_id"], "client-a")
        self.assertEqual(payload["events"][0]["raw"]["msg"], "ready")

    def test_since_filters_by_message_timestamp(self) -> None:
        since = datetime(2026, 5, 16, 1, 0, 0, tzinfo=timezone.utc)

        events = logs.events_from_lines(
            [
                self.line(ts="2026-05-16T00:59:59Z", **{"from": "agent"}, msg="old"),
                self.line(ts="2026-05-16T01:00:01Z", **{"from": "agent"}, msg="new"),
            ],
            since=since,
        )

        self.assertEqual([event.msg for event in events], ["new"])

    def test_cli_json_mode(self) -> None:
        stdin = io.StringIO(self.line(sig="sig-1", ts="2026-05-16T01:00:00Z", **{"from": "agent"}, msg="ready"))
        stdout = io.StringIO()

        with patch("sys.stdin", stdin), redirect_stdout(stdout):
            code = logs.main(["render", "--count", "20", "--json"])

        self.assertEqual(code, 0)
        payload = json.loads(stdout.getvalue())
        self.assertEqual(payload["events"][0]["id"], "sig-1")

    def test_airc_logs_human_mode_does_not_require_json_flag(self) -> None:
        result = self.run_airc_logs("1")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("[2026-05-16T01:00:00Z] agent: ready", result.stdout)

    def test_airc_logs_json_mode_still_emits_machine_readable_page(self) -> None:
        result = self.run_airc_logs("1", "--json")

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout[result.stdout.index("{"):])
        self.assertEqual(payload["count"], 1)
        self.assertEqual(payload["events"][0]["sender"], "agent")

    def run_airc_logs(self, *args: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as home:
            Path(home, "messages.jsonl").write_text(
                self.line(sig="sig-1", ts="2026-05-16T01:00:00Z", **{"from": "agent"}, msg="ready"),
                encoding="utf-8",
            )
            env = {**os.environ, "AIRC_HOME": home}
            return subprocess.run(
                [str(REPO_ROOT / "airc"), "logs", *args],
                cwd=REPO_ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )


if __name__ == "__main__":
    unittest.main()
