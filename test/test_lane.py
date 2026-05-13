"""Tests for `airc lane` — worktree lanes for multi-agent safety."""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"


def run_airc(args: list[str], env: dict[str, str], cwd: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(AIRC_BIN), *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=cwd or str(REPO_ROOT),
        timeout=20,
    )


def isolated_env(tmp: str) -> dict[str, str]:
    env = os.environ.copy()
    env.update({
        "HOME": tmp,
        "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
        "AIRC_NO_IDENTITY_PROMPT": "1",
    })
    return env


def make_git_repo(tmp: str) -> pathlib.Path:
    repo = pathlib.Path(tmp) / "repo"
    repo.mkdir()
    subprocess.run(["git", "init"], cwd=repo, check=True, capture_output=True, text=True)
    subprocess.run(["git", "config", "user.email", "airc-test@example.invalid"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "AIRC Test"], cwd=repo, check=True)
    (repo / "README.md").write_text("test\n", encoding="utf-8")
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-m", "init"], cwd=repo, check=True, capture_output=True, text=True)
    subprocess.run(["git", "checkout", "-b", "canary"], cwd=repo, check=True, capture_output=True, text=True)
    return repo


class LaneCommandTests(unittest.TestCase):
    def test_lane_help_mentions_canary_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["lane", "--help"], isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("worktree", result.stdout)
        self.assertIn("canary", result.stdout)

    def test_lane_create_dry_run_defaults_to_canary_and_safe_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = make_git_repo(tmp)
            result = run_airc(
                ["lane", "create", "CambrianTech/airc#584",
                 "--repo", str(repo), "--owner", "codex", "--dry-run"],
                isolated_env(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DRY RUN", result.stdout)
        self.assertIn("base:   canary", result.stdout)
        self.assertIn("branch: feat/cambriantech-airc-584-codex", result.stdout)
        self.assertIn(".airc-worktrees", result.stdout)

    def test_lane_create_rejects_dir_inside_protected_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = make_git_repo(tmp)
            result = run_airc(
                ["lane", "create", "CambrianTech/airc#584",
                 "--repo", str(repo),
                 "--dir", str(repo / "nested-lane"),
                 "--owner", "codex"],
                isolated_env(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("protected checkout", result.stdout + result.stderr)

    def test_lane_create_records_and_remove_removes_worktree(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = make_git_repo(tmp)
            lane_dir = pathlib.Path(tmp) / "lanes" / "airc-584"
            env = isolated_env(tmp)
            result = run_airc(
                ["lane", "create", "CambrianTech/airc#584",
                 "--repo", str(repo),
                 "--dir", str(lane_dir),
                 "--branch", "feat/worktree-lanes-test",
                 "--owner", "codex"],
                env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((lane_dir / ".git").exists())
            registry = pathlib.Path(env["AIRC_HOME"]) / "lanes.jsonl"
            self.assertTrue(registry.exists())
            lane = json.loads(registry.read_text(encoding="utf-8").strip().splitlines()[-1])
            self.assertEqual(lane["issue"], "CambrianTech/airc#584")
            self.assertEqual(lane["branch"], "feat/worktree-lanes-test")
            self.assertEqual(lane["base"], "canary")

            listed = run_airc(["lane", "list"], env)
            self.assertEqual(listed.returncode, 0, listed.stderr)
            self.assertIn("CambrianTech/airc#584", listed.stdout)

            removed = run_airc(["lane", "remove", "CambrianTech/airc#584"], env)
            self.assertEqual(removed.returncode, 0, removed.stderr)
            self.assertFalse(lane_dir.exists())


if __name__ == "__main__":
    unittest.main()
