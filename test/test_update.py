"""Tests for `airc update` install-dir conflict safety."""

from __future__ import annotations

import os
import pathlib
import stat
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"


def run_airc(args: list[str], env: dict[str, str], cwd: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(AIRC_BIN), *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=cwd,
        timeout=20,
    )


def _git(repo: pathlib.Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=repo,
        check=check,
        capture_output=True,
        text=True,
    )


def isolated_env(tmp: str, install_dir: pathlib.Path) -> dict[str, str]:
    env = os.environ.copy()
    env.update({
        "HOME": tmp,
        "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
        "AIRC_DIR": str(install_dir),
        "AIRC_NO_IDENTITY_PROMPT": "1",
    })
    return env


def make_install_repo(tmp: str) -> tuple[pathlib.Path, pathlib.Path]:
    origin = pathlib.Path(tmp) / "origin"
    origin.mkdir()
    _git(origin, "init", "-b", "canary")
    _git(origin, "config", "user.email", "airc-test@example.invalid")
    _git(origin, "config", "user.name", "AIRC Test")
    (origin / "README.md").write_text("base\n", encoding="utf-8")
    install = origin / "install.sh"
    install.write_text(
        "#!/usr/bin/env bash\n"
        "cd \"$(dirname \"$0\")\"\n"
        "[ -n \"${AIRC_UPDATE_TEST_MARKER:-}\" ] && echo ran >> \"$AIRC_UPDATE_TEST_MARKER\"\n"
        "[ -n \"${AIRC_UPDATE_TEST_DIRTY:-}\" ] && echo dirty > README.md\n"
        "exit 0\n",
        encoding="utf-8",
    )
    install.chmod(install.stat().st_mode | stat.S_IXUSR)
    _git(origin, "add", "README.md", "install.sh")
    _git(origin, "commit", "-m", "base")

    clone = pathlib.Path(tmp) / "install"
    _git(pathlib.Path(tmp), "clone", str(origin), str(clone))
    _git(clone, "checkout", "canary")
    (clone / ".channel").write_text("canary\n", encoding="utf-8")
    return origin, clone


def create_unmerged_conflict(repo: pathlib.Path) -> None:
    _git(repo, "checkout", "-b", "other")
    (repo / "README.md").write_text("other\n", encoding="utf-8")
    _git(repo, "commit", "-am", "other change")
    _git(repo, "checkout", "canary")
    (repo / "README.md").write_text("local\n", encoding="utf-8")
    _git(repo, "commit", "-am", "local change")
    merge = _git(repo, "merge", "other", check=False)
    if merge.returncode == 0:
        raise AssertionError("expected merge conflict")


class UpdateConflictTests(unittest.TestCase):
    def test_update_refuses_existing_unmerged_install_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            _origin, install_dir = make_install_repo(tmp)
            create_unmerged_conflict(install_dir)
            marker = pathlib.Path(tmp) / "install-ran"
            env = isolated_env(tmp, install_dir)
            env["AIRC_UPDATE_TEST_MARKER"] = str(marker)

            result = run_airc(["update"], env, str(REPO_ROOT))

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Unresolved git merge conflicts", result.stderr)
            self.assertIn("airc update --reset", result.stderr)
            self.assertFalse(marker.exists(), "install.sh must not run while install dir is conflicted")
            self.assertIn("UU README.md", _git(install_dir, "status", "--short").stdout)

    def test_update_reset_recovers_conflicted_install_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            _origin, install_dir = make_install_repo(tmp)
            create_unmerged_conflict(install_dir)
            marker = pathlib.Path(tmp) / "install-ran"
            env = isolated_env(tmp, install_dir)
            env["AIRC_UPDATE_TEST_MARKER"] = str(marker)

            result = run_airc(["update", "--reset"], env, str(REPO_ROOT))

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Resetting install dir to origin/canary", result.stdout)
            self.assertTrue(marker.exists(), "install.sh should run after reset recovery")
            self.assertEqual(_git(install_dir, "status", "--short", "--untracked-files=no").stdout, "")
            self.assertEqual((install_dir / "README.md").read_text(encoding="utf-8"), "base\n")

    def test_update_refuses_post_install_tracked_changes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            _origin, install_dir = make_install_repo(tmp)
            marker = pathlib.Path(tmp) / "install-ran"
            env = isolated_env(tmp, install_dir)
            env["AIRC_UPDATE_TEST_MARKER"] = str(marker)
            env["AIRC_UPDATE_TEST_DIRTY"] = "1"

            result = run_airc(["update"], env, str(REPO_ROOT))

            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(marker.exists(), "install.sh should have run before post-update guard")
            self.assertIn("install dir has tracked local changes", result.stderr)
            self.assertIn("airc update --reset", result.stderr)
            self.assertIn("M README.md", _git(install_dir, "status", "--short").stdout)


if __name__ == "__main__":
    unittest.main()
