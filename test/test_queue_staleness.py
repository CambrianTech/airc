"""Tests for `airc queue staleness` (airc#615).

The command is a git-side reviewer guard: a PR can pass CI while its branch
is stale relative to base and would erase already-merged lines. These tests
use real local git repos with --no-fetch so the topology is genuine without
network or GitHub auth.
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
        capture_output=True,
        text=True,
        env=env,
        cwd=str(REPO_ROOT),
        timeout=20,
    )


def _isolated_env(tmp: str) -> dict[str, str]:
    return {
        "HOME": tmp,
        "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
        "AIRC_NO_IDENTITY_PROMPT": "1",
        "PATH": "/usr/bin:/bin",
    }


def _git(repo: pathlib.Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True)


def _commit(repo: pathlib.Path, message: str) -> None:
    _git(repo, "add", ".")
    _git(repo, "commit", "-m", message)


def _make_repo(tmp: str) -> pathlib.Path:
    repo = pathlib.Path(tmp) / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-b", "base", str(repo)], check=True, capture_output=True, text=True)
    _git(repo, "config", "user.email", "airc@example.invalid")
    _git(repo, "config", "user.name", "AIRC Test")
    (repo / "src").mkdir()
    (repo / "src" / "widget.ts").write_text("export const value = 'initial';\n", encoding="utf-8")
    _commit(repo, "initial")
    return repo


def _queue_card_body(status: str, text: str) -> str:
    return "**airc-queue card**\n\n```json\n" + json.dumps({
        "kind": "airc-queue-card-v1",
        "status": status,
        "owner": "airc-test",
        "next_action": text,
    }, indent=2) + "\n```\n"


def _env_with_list_fake_gh(tmp: str, issues: list[dict[str, object]]) -> dict[str, str]:
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir(exist_ok=True)
    issues_file = pathlib.Path(tmp) / "issues.json"
    issues_file.write_text(json.dumps(issues), encoding="utf-8")
    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        f'ISSUES_FILE="{issues_file}"\n'
        'case "$1 $2" in\n'
        '  "issue list") cat "$ISSUES_FILE" ;;\n'
        '  "pr view") printf \'{"baseRefName":"base","headRefName":"pr","url":"https://github.com/owner/repo/pull/77","title":"test"}\' ;;\n'
        '  *) printf "[]" ;;\n'
        'esac\n',
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env


class QueueStalenessTests(unittest.TestCase):
    def test_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "staleness", "--help"], _isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--repo-root", result.stdout)
        self.assertIn("--no-fetch", result.stdout)

    def test_top_level_help_advertises_staleness(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "--help"], _isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("staleness", result.stdout)
        self.assertIn("airc#615", result.stdout)

    def test_list_help_documents_staleness_sweep(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "list", "--help"], _isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--check-staleness", result.stdout)
        self.assertIn("--repo-root", result.stdout)

    def test_warns_when_pr_head_lacks_current_base_line_in_touched_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_repo(tmp)

            _git(repo, "checkout", "-b", "pr")
            (repo / "src" / "widget.ts").write_text("export const value = 'from-pr';\n", encoding="utf-8")
            _commit(repo, "pr changes widget")

            _git(repo, "checkout", "base")
            (repo / "src" / "widget.ts").write_text(
                "export const value = 'initial';\n"
                "export const xssHardening = true;\n",
                encoding="utf-8",
            )
            _commit(repo, "add #1100 xss hardening")

            result = run_airc(
                [
                    "queue",
                    "staleness",
                    "--repo-root",
                    str(repo),
                    "--base",
                    "base",
                    "--head",
                    "pr",
                    "--no-fetch",
                ],
                _isolated_env(tmp),
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("WARN:", result.stdout)
        self.assertIn("xssHardening", result.stdout)
        self.assertIn("#1100", result.stdout)

    def test_json_reports_no_warnings_for_fresh_branch(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_repo(tmp)

            _git(repo, "checkout", "base")
            (repo / "src" / "widget.ts").write_text(
                "export const value = 'initial';\n"
                "export const xssHardening = true;\n",
                encoding="utf-8",
            )
            _commit(repo, "add #1100 xss hardening")

            _git(repo, "checkout", "-b", "pr")
            (repo / "src" / "widget.ts").write_text(
                "export const value = 'from-pr';\n"
                "export const xssHardening = true;\n",
                encoding="utf-8",
            )
            _commit(repo, "pr changes widget after rebase")

            result = run_airc(
                [
                    "queue",
                    "staleness",
                    "--repo-root",
                    str(repo),
                    "--base",
                    "base",
                    "--head",
                    "pr",
                    "--no-fetch",
                    "--json",
                ],
                _isolated_env(tmp),
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["warning_count"], 0)
        self.assertEqual(payload["warnings"], [])

    def test_list_check_staleness_runs_for_review_cards(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_repo(tmp)

            _git(repo, "checkout", "-b", "pr")
            (repo / "src" / "widget.ts").write_text("export const value = 'from-pr';\n", encoding="utf-8")
            _commit(repo, "pr changes widget")

            _git(repo, "checkout", "base")
            (repo / "src" / "widget.ts").write_text(
                "export const value = 'initial';\n"
                "export const xssHardening = true;\n",
                encoding="utf-8",
            )
            _commit(repo, "add #1100 xss hardening")

            issues = [{
                "number": 1,
                "title": "review card",
                "url": "https://github.com/owner/repo/issues/1",
                "createdAt": "2026-05-14T00:00:00Z",
                "updatedAt": "2026-05-14T00:00:00Z",
                "body": _queue_card_body("review", "PR owner/repo#77 needs branch staleness check"),
            }]
            env = _env_with_list_fake_gh(tmp, issues)
            result = run_airc(
                [
                    "queue",
                    "list",
                    "owner/repo",
                    "--check-staleness",
                    "--repo-root",
                    str(repo),
                    "--no-fetch-staleness",
                ],
                env,
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("# staleness sweep", result.stdout)
        self.assertIn("WARN:", result.stdout)
        self.assertIn("xssHardening", result.stdout)


if __name__ == "__main__":
    unittest.main()
