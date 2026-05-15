"""Tests for `airc hygiene` workspace cache policy."""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"


def run_airc(
    args: list[str],
    env: dict[str, str],
    cwd: str,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(AIRC_BIN), *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=cwd,
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
    return repo


class HygieneCommandTests(unittest.TestCase):
    def test_init_writes_serde_friendly_policy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = make_git_repo(tmp)
            result = run_airc(["hygiene", "init"], isolated_env(tmp), str(repo))

            self.assertEqual(result.returncode, 0, result.stderr)
            policy_path = repo / ".airc-policy.json"
            self.assertTrue(policy_path.exists())
            policy = json.loads(policy_path.read_text(encoding="utf-8"))
            self.assertEqual(policy["workspace_root"], "~/.airc-worktrees")
            self.assertTrue(policy["clean_worktree_rust_targets"])
            self.assertTrue(policy["clean_worktree_node_modules"])
            self.assertFalse(policy["clean_main_rust_target"])

    def test_report_lists_only_policy_candidates(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = make_git_repo(tmp)
            workspace = pathlib.Path(tmp) / "lanes"
            target = workspace / "repo-card-agent" / "src" / "workers" / "target"
            modules = workspace / "repo-card-agent" / "src" / "node_modules"
            protected = repo / "src" / "workers" / "target"
            for path in (target, modules, protected):
                path.mkdir(parents=True)
                (path / "sentinel").write_text("cache\n", encoding="utf-8")
            (repo / ".airc-policy.json").write_text(
                json.dumps({"workspace_root": str(workspace)}, indent=2),
                encoding="utf-8",
            )

            result = run_airc(["hygiene", "report"], isolated_env(tmp), str(repo))

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("worktree-rust-target", result.stdout)
            self.assertIn("worktree-node-modules", result.stdout)
            self.assertIn(str(target), result.stdout)
            self.assertIn(str(modules), result.stdout)
            self.assertNotIn(str(protected), result.stdout)

    def test_clean_requires_yes_or_dry_run(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = make_git_repo(tmp)
            workspace = pathlib.Path(tmp) / "lanes"
            target = workspace / "repo-card-agent" / "src" / "workers" / "target"
            target.mkdir(parents=True)
            (target / "sentinel").write_text("cache\n", encoding="utf-8")
            (repo / ".airc-policy.json").write_text(
                json.dumps({"workspace_root": str(workspace)}, indent=2),
                encoding="utf-8",
            )

            result = run_airc(["hygiene", "clean"], isolated_env(tmp), str(repo))

            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(target.exists())
            self.assertIn("--yes or --dry-run", result.stderr)

    def test_clean_yes_removes_rebuildable_worktree_caches(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = make_git_repo(tmp)
            workspace = pathlib.Path(tmp) / "lanes"
            target = workspace / "repo-card-agent" / "src" / "workers" / "target"
            modules = workspace / "repo-card-agent" / "src" / "node_modules"
            for path in (target, modules):
                path.mkdir(parents=True)
                (path / "sentinel").write_text("cache\n", encoding="utf-8")
            (repo / ".airc-policy.json").write_text(
                json.dumps({"workspace_root": str(workspace)}, indent=2),
                encoding="utf-8",
            )

            result = run_airc(["hygiene", "clean", "--yes"], isolated_env(tmp), str(repo))

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(target.exists())
            self.assertFalse(modules.exists())


if __name__ == "__main__":
    unittest.main()
