"""GitHub request guard tests.

Run: cd test && python3 test_gh_backoff.py
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "lib"))

from airc_core import gh_backoff  # noqa: E402


class GhGuardTests(unittest.TestCase):
    def test_budget_blocks_before_subprocess_and_audits(self):
        with tempfile.TemporaryDirectory() as tmp, \
             mock.patch.object(tempfile, "gettempdir", return_value=tmp), \
             mock.patch.dict("os.environ", {
                 "AIRC_GH_MAX_REQUESTS_PER_MIN": "1",
                 "AIRC_GH_AUDIT_LOG": str(Path(tmp) / "audit.jsonl"),
             }, clear=False), \
             mock.patch.object(gh_backoff.subprocess, "run") as run:
            run.return_value = gh_backoff.subprocess.CompletedProcess(
                ["gh", "api", "rate_limit"], 0, "{}", ""
            )

            first = gh_backoff.run_gh("gh", ["api", "rate_limit"])
            second = gh_backoff.run_gh("gh", ["api", "gists?per_page=1"])

            self.assertEqual(first.returncode, 0)
            self.assertEqual(second.returncode, 75)
            self.assertIn("local request budget exceeded", second.stderr)
            self.assertEqual(run.call_count, 1)
            self.assertTrue(gh_backoff.backoff_active())

            events = [
                json.loads(line)
                for line in Path(tmp, "audit.jsonl").read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(events[-1]["outcome"], "blocked")
            self.assertEqual(events[-1]["class"], "api:gists")

    def test_backoff_blocks_before_subprocess_and_audits(self):
        with tempfile.TemporaryDirectory() as tmp, \
             mock.patch.object(tempfile, "gettempdir", return_value=tmp), \
             mock.patch.dict("os.environ", {
                 "AIRC_GH_AUDIT_LOG": str(Path(tmp) / "audit.jsonl"),
             }, clear=False), \
             mock.patch.object(gh_backoff.subprocess, "run") as run:
            gh_backoff.record_backoff("retry-after: 120")
            result = gh_backoff.run_gh("gh", ["gist", "list", "--limit", "1"])

            self.assertEqual(result.returncode, 75)
            self.assertIn("shared backoff active", result.stderr)
            run.assert_not_called()

            event = json.loads(Path(tmp, "audit.jsonl").read_text(encoding="utf-8").splitlines()[-1])
            self.assertEqual(event["class"], "gist:list")
            self.assertFalse(event["allowed"])

    def test_unguarded_command_passes_through(self):
        with tempfile.TemporaryDirectory() as tmp, \
             mock.patch.object(tempfile, "gettempdir", return_value=tmp), \
             mock.patch.object(gh_backoff.subprocess, "run") as run:
            run.return_value = gh_backoff.subprocess.CompletedProcess(
                ["gh", "version"], 0, "gh version", ""
            )
            result = gh_backoff.run_gh("gh", ["version"])

            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "gh version")
            run.assert_called_once()

    def test_audit_reset_clears_backoff_and_budget_not_audit(self):
        with tempfile.TemporaryDirectory() as tmp, \
             mock.patch.object(tempfile, "gettempdir", return_value=tmp), \
             mock.patch.dict("os.environ", {
                 "AIRC_GH_AUDIT_LOG": str(Path(tmp) / "audit.jsonl"),
             }, clear=False):
            gh_backoff.record_backoff("retry-after: 120")
            Path(gh_backoff.budget_path()).write_text("1\n2\n", encoding="utf-8")
            Path(gh_backoff.audit_path()).write_text('{"x":1}\n', encoding="utf-8")

            rc = gh_backoff._main(["audit", "--reset"])

            self.assertEqual(rc, 0)
            self.assertFalse(Path(gh_backoff.backoff_path()).exists())
            self.assertFalse(Path(gh_backoff.budget_path()).exists())
            self.assertTrue(Path(gh_backoff.audit_path()).exists())


if __name__ == "__main__":
    unittest.main()
