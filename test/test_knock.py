"""Tests for `airc knock` — public collaboration knock entrypoint (airc#559 PR-1).

Coverage:
  - dispatch: `airc knock` reaches cmd_knock and the help path works
  - validation: missing target / missing message / malformed target
  - dry-run: prints the would-be envelope, does not call `gh`
  - envelope shape: title prefix + JSON identity + message included
  - fallback identity: works even with no identity.json

The actual `gh issue create` invocation is NOT exercised here (would require
a real GitHub repo + auth). The dry-run path proves the envelope shape and
the parameter wiring up to that boundary; an integration test in
test/integration.sh can do the live gh call later.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"


def run_airc(args: list[str], env_overrides: dict[str, str] | None = None,
             cwd: str | None = None) -> subprocess.CompletedProcess[str]:
    """Run `airc <args>` and return the completed process.

    Uses a temp HOME so the test never touches the real ~/.airc, and a
    temp AIRC_HOME so identity / state from any real install can't leak in.
    """
    env = os.environ.copy()
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        [str(AIRC_BIN), *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=cwd or str(REPO_ROOT),
        timeout=15,
    )


class KnockDispatchTests(unittest.TestCase):
    """The knock verb must reach cmd_knock without hitting cmd_send etc."""

    def test_knock_help_returns_zero_and_mentions_owner_repo(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = {"HOME": tmp, "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                   "AIRC_NO_IDENTITY_PROMPT": "1"}
            result = run_airc(["knock", "--help"], env_overrides=env)
        self.assertEqual(result.returncode, 0,
                         f"knock --help should succeed; stderr={result.stderr}")
        self.assertIn("owner/repo", result.stdout)
        self.assertIn("airc-knock", result.stdout)


class KnockValidationTests(unittest.TestCase):
    """Invalid inputs must fail loudly with a useful message."""

    def test_missing_target_fails_with_usage(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = {"HOME": tmp, "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                   "AIRC_NO_IDENTITY_PROMPT": "1"}
            result = run_airc(["knock"], env_overrides=env)
        self.assertNotEqual(result.returncode, 0,
                            "knock with no args must fail")
        # Usage hint appears on stderr (the help path returns 1 when called
        # without args, printing the help text to stderr).
        combined = result.stdout + result.stderr
        self.assertIn("owner/repo", combined)

    def test_bare_project_name_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = {"HOME": tmp, "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                   "AIRC_NO_IDENTITY_PROMPT": "1"}
            result = run_airc(["knock", "continuum", "hi"], env_overrides=env)
        self.assertNotEqual(result.returncode, 0,
                            "bare project name (no owner/) must fail")
        self.assertIn("owner/repo", result.stdout + result.stderr)

    def test_missing_message_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = {"HOME": tmp, "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                   "AIRC_NO_IDENTITY_PROMPT": "1"}
            result = run_airc(["knock", "CambrianTech/continuum"],
                              env_overrides=env)
        self.assertNotEqual(result.returncode, 0,
                            "knock with target but no message must fail")
        self.assertIn("message", result.stdout + result.stderr)


class KnockDryRunTests(unittest.TestCase):
    """--dry-run emits the envelope and does not call gh."""

    def test_dry_run_prints_envelope_with_message(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = {"HOME": tmp, "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                   "AIRC_NO_IDENTITY_PROMPT": "1",
                   # Force PATH that doesn't have gh so we PROVE the
                   # function returned before reaching the gh call.
                   # If --dry-run still invokes gh, this test fails by
                   # surfacing 'gh not found' or similar.
                   "PATH": "/usr/bin:/bin"}
            result = run_airc(
                ["knock", "CambrianTech/continuum",
                 "--dry-run",
                 "--message", "test envelope for knock"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0,
                         f"dry-run should succeed; stderr={result.stderr}")
        self.assertIn("DRY RUN", result.stdout)
        self.assertIn("CambrianTech/continuum", result.stdout)
        self.assertIn("test envelope for knock", result.stdout)
        self.assertIn("airc-knock:", result.stdout)

    def test_dry_run_includes_json_identity_envelope(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = {"HOME": tmp, "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                   "AIRC_NO_IDENTITY_PROMPT": "1",
                   "PATH": "/usr/bin:/bin"}
            result = run_airc(
                ["knock", "owner/repo", "--dry-run", "-m", "hello"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        # JSON envelope is in the body; extract and parse it.
        # Dry-run prints body lines indented (sed '    ' prefix) — match
        # the JSON block regardless of leading whitespace per line.
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          result.stdout, re.DOTALL)
        self.assertIsNotNone(
            match,
            f"expected JSON identity block in dry-run output, got:\n{result.stdout}")
        envelope = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertIn("name", envelope)
        self.assertIn("pronouns", envelope)
        self.assertIn("role", envelope)
        self.assertIn("bio", envelope)
        self.assertIn("gh_login", envelope)


class KnockTitleSlicingTests(unittest.TestCase):
    """Long messages get sliced to fit GitHub's 256-char issue-title limit."""

    def test_long_message_title_is_truncated(self) -> None:
        long_message = "x" * 500
        with tempfile.TemporaryDirectory() as tmp:
            env = {"HOME": tmp, "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
                   "AIRC_NO_IDENTITY_PROMPT": "1",
                   "PATH": "/usr/bin:/bin"}
            result = run_airc(
                ["knock", "owner/repo", "--dry-run", "-m", long_message],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        # Find the title line and verify it's bounded.
        title_match = re.search(r'title:\s+(.+)', result.stdout)
        self.assertIsNotNone(title_match)
        title = title_match.group(1)  # type: ignore[union-attr]
        self.assertLess(len(title), 256,
                        f"title must fit GH 256-char limit; got {len(title)}")
        self.assertTrue(title.endswith("..."),
                        f"long titles must end with ellipsis; got: {title}")


if __name__ == "__main__":
    unittest.main()
