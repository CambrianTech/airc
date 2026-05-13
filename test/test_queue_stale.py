"""Tests for `airc queue stale` (airc#572).

The command is intentionally read-only: it reports missing/old heartbeat
state so humans/agents can nudge, heartbeat, or release claims without
automated claim stealing in the first PR.
"""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"


def run_airc(args: list[str], env_overrides: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        [str(AIRC_BIN), *args],
        capture_output=True, text=True, env=env,
        cwd=str(REPO_ROOT), timeout=15,
    )


def _isolated_env(tmp: str) -> dict[str, str]:
    return {
        "HOME": tmp,
        "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
        "AIRC_NO_IDENTITY_PROMPT": "1",
        "PATH": "/usr/bin:/bin",
    }


def _card(status: str, owner: str = "", heartbeat: str = "") -> str:
    card = {
        "kind": "airc-queue-card-v1",
        "status": status,
        "branch": "feat/x",
        "next_action": "continue work",
    }
    if owner:
        card["owner"] = owner
    if heartbeat:
        card["last_heartbeat"] = heartbeat
    return "**airc-queue card**\n\n```json\n" + json.dumps(card, indent=2) + "\n```\n"


def _env_with_fake_gh(tmp: str) -> dict[str, str]:
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir(exist_ok=True)
    issues = [
        {
            "number": 1,
            "title": "old active claim",
            "url": "https://github.com/owner/repo/issues/1",
            "updatedAt": "2026-05-13T20:00:00Z",
            "body": _card("in-progress", "claude-tab-1", "2000-01-01T00:00Z @ deadbee"),
        },
        {
            "number": 2,
            "title": "missing heartbeat",
            "url": "https://github.com/owner/repo/issues/2",
            "updatedAt": "2026-05-13T20:00:00Z",
            "body": _card("review", "codex"),
        },
        {
            "number": 3,
            "title": "missing owner",
            "url": "https://github.com/owner/repo/issues/3",
            "updatedAt": "2026-05-13T20:00:00Z",
            "body": _card("claimed"),
        },
        {
            "number": 4,
            "title": "done",
            "url": "https://github.com/owner/repo/issues/4",
            "updatedAt": "2026-05-13T20:00:00Z",
            "body": _card("merged", "codex"),
        },
    ]
    issues_file = pathlib.Path(tmp) / "issues.json"
    issues_file.write_text(json.dumps(issues), encoding="utf-8")

    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        f'ISSUES_FILE="{issues_file}"\n'
        "case \"$1 $2\" in\n"
        "  'issue list') cat \"$ISSUES_FILE\" ;;\n"
        "  *) echo '[]' ;;\n"
        "esac\n",
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env


class QueueStaleTests(unittest.TestCase):
    def test_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "stale", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--stale-after", result.stdout)
        self.assertIn("read-only", result.stdout.lower())

    def test_stale_lists_missing_and_old_heartbeats(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "stale", "owner/repo", "--stale-after", "1m"],
                env_overrides=_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("#1", result.stdout)
        self.assertIn("stale-heartbeat", result.stdout)
        self.assertIn("#2", result.stdout)
        self.assertIn("missing-heartbeat", result.stdout)
        self.assertIn("#3", result.stdout)
        self.assertIn("missing-owner", result.stdout)
        self.assertNotIn("#4", result.stdout)

    def test_stale_json_shape(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "stale", "owner/repo", "--stale-after", "1m", "--json"],
                env_overrides=_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["repo"], "owner/repo")
        self.assertEqual(len(payload["cards"]), 3)
        self.assertEqual(payload["cards"][0]["reason"], "stale-heartbeat")


if __name__ == "__main__":
    unittest.main()
