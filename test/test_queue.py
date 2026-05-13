"""Tests for `airc queue` — issue-backed work queue primitives (airc#562 PR-1).

Coverage:
  - dispatch: subcommand router + add/list reach the right functions + --help paths work
  - validation: missing args / bad status enum / malformed repo all fail loud
  - card body shape: dry-run output embeds a JSON envelope with kind=airc-queue-card-v1
  - default owner: falls back to resolve_name when --owner omitted
  - field threading: every --flag ends up in the card JSON
  - auto-detect: queue list with no <owner/repo> uses git remote
  - status enum: only canonical states accepted

The actual `gh issue create` + `gh issue list` invocations are NOT exercised
(they would need a real GitHub repo + auth). cmd_queue add's --dry-run path
covers everything up to the gh call; list shape is contract-tested separately
by inspecting the JSON envelope a dry-run add would emit and matching the
parser regex used by list's python filter.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"


def run_airc(args: list[str], env_overrides: dict[str, str] | None = None,
             cwd: str | None = None) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        [str(AIRC_BIN), *args],
        capture_output=True, text=True, env=env,
        cwd=cwd or str(REPO_ROOT), timeout=15,
    )


def _isolated_env(tmp: str) -> dict[str, str]:
    return {
        "HOME": tmp,
        "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
        "AIRC_NO_IDENTITY_PROMPT": "1",
        "PATH": "/usr/bin:/bin",
    }


def _isolated_env_with_fake_gh(tmp: str) -> dict[str, str]:
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir()
    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' '[]'\n",
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env


class QueueDispatchTests(unittest.TestCase):
    def test_queue_no_subcommand_prints_help(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue"], env_overrides=_isolated_env(tmp))
        # No subcommand → returncode 1 (caller needed help, didn't ask explicitly).
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("queue add", result.stdout + result.stderr)
        self.assertIn("queue list", result.stdout + result.stderr)

    def test_queue_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "--help"], env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("PR-1", result.stdout)

    def test_queue_add_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "add", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--title", result.stdout)
        self.assertIn("--status", result.stdout)

    def test_queue_list_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "list", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--owner", result.stdout)

    def test_unknown_subcommand_fails_loudly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "frobnicate"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown subcommand", result.stdout + result.stderr)


class QueueAddValidationTests(unittest.TestCase):
    def test_missing_repo_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "add", "--title", "x"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)

    def test_missing_title_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "add", "owner/repo"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--title", result.stdout + result.stderr)

    def test_bare_project_name_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "add", "bare-project", "--title", "x"],
                env_overrides=_isolated_env(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("owner/repo", result.stdout + result.stderr)

    def test_bad_status_rejected_with_canonical_list(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "add", "owner/repo",
                 "--title", "x", "--status", "in-flight"],
                env_overrides=_isolated_env(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        combined = result.stdout + result.stderr
        # Error must NAME the canonical values so operator can fix.
        for canonical in ("claimed", "in-progress", "blocked", "review", "merged"):
            self.assertIn(canonical, combined,
                          f"error must list canonical state '{canonical}'")


class QueueAddCardBodyTests(unittest.TestCase):
    """--dry-run emits the issue body that would be posted. Verify shape."""

    def _dry_run(self, *extra_args: str) -> str:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "add", "owner/repo",
                 "--title", "test card",
                 "--dry-run", *extra_args],
                env_overrides=_isolated_env(tmp),
            )
        self.assertEqual(result.returncode, 0,
                         f"dry-run must succeed; stderr={result.stderr}")
        return result.stdout

    def test_dry_run_emits_kind_envelope(self) -> None:
        out = self._dry_run("--owner", "claude-tab-2", "--branch", "feat/x")
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          out, re.DOTALL)
        self.assertIsNotNone(match, f"expected JSON card block; got:\n{out}")
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card.get("kind"), "airc-queue-card-v1")
        self.assertEqual(card.get("owner"), "claude-tab-2")
        self.assertEqual(card.get("branch"), "feat/x")

    def test_dry_run_threads_all_fields(self) -> None:
        out = self._dry_run(
            "--id", "#1085",
            "--branch", "fix/install-tier",
            "--owner", "codex",
            "--status", "in-progress",
            "--blockers", "#1071, airc#559",
            "--env", "linux-amd64-any",
            "--evidence", "prepush green",
            "--next-action", "wait for image push",
            "--last-heartbeat", "2026-05-13T19:00Z @ abc123",
        )
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          out, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card["id"], "#1085")
        self.assertEqual(card["branch"], "fix/install-tier")
        self.assertEqual(card["owner"], "codex")
        self.assertEqual(card["status"], "in-progress")
        self.assertEqual(card["blockers"], "#1071, airc#559")
        self.assertEqual(card["env"], "linux-amd64-any")
        self.assertEqual(card["evidence"], "prepush green")
        self.assertEqual(card["next_action"], "wait for image push")
        self.assertEqual(card["last_heartbeat"], "2026-05-13T19:00Z @ abc123")

    def test_dry_run_default_status_is_claimed(self) -> None:
        out = self._dry_run("--owner", "anon")
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          out, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card["status"], "claimed",
                         "queue add must default to status=claimed")

    def test_dry_run_default_owner_is_resolved_name(self) -> None:
        # No --owner → owner field falls back to resolve_name (which
        # falls back to derive_name → hostname). Must be non-empty.
        out = self._dry_run()
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          out, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertIn("owner", card)
        self.assertGreater(len(card["owner"]), 0,
                           "default owner must resolve to SOMETHING")


class QueueListAutoDetectTests(unittest.TestCase):
    """`airc queue list` with no <owner/repo> must auto-detect from cwd's
    git remote — when present. Fails clearly when it can't."""

    def test_list_outside_git_repo_fails_with_hint(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            # tmp is not a git repo
            result = run_airc(["queue", "list"],
                              env_overrides=_isolated_env(tmp),
                              cwd=tmp)
        self.assertNotEqual(result.returncode, 0)
        combined = result.stdout + result.stderr
        self.assertIn("owner/repo", combined,
                      "missing-repo error must hint at the right arg shape")

    def test_list_json_includes_now_utc_anchor(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "list", "owner/repo", "--json"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertRegex(payload["now_utc"], r"^\d{4}-\d{2}-\d{2}T")
        self.assertEqual(payload["repo"], "owner/repo")
        self.assertEqual(payload["cards"], [])

    def test_list_human_includes_now_utc_anchor(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "list", "owner/repo"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("now_utc:", result.stdout)
        self.assertIn("No open airc-queue cards", result.stdout)


if __name__ == "__main__":
    unittest.main()
