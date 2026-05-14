"""Tests for `airc queue` — issue-backed work queue primitives (airc#562 PR-1).

Coverage:
  - dispatch: subcommand router + add/list reach the right functions + --help paths work
  - validation: missing args / bad status enum / malformed repo all fail loud
  - card body shape: dry-run output embeds a JSON envelope with kind=airc-queue-card-v1
  - default owner: session/work identity, then compatibility env fallback
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


def _queue_card_body(**fields: str) -> str:
    card = {"kind": "airc-queue-card-v1", **fields}
    return (
        "**airc-queue card**\n\n"
        "```json\n"
        f"{json.dumps(card, indent=2)}\n"
        "```\n"
    )


def _isolated_env_with_plan_fake_gh(tmp: str) -> dict[str, str]:
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir()
    issue_list = [
        {
            "number": 11,
            "title": "Move persona cognition planner into Rust",
            "url": "https://github.com/owner/repo/issues/11",
            "body": _queue_card_body(
                status="claimed",
                next_action="Define Rust trait boundary and ts-rs export.",
                env="any",
            ),
            "createdAt": "2026-05-14T10:00:00Z",
            "updatedAt": "2026-05-14T10:05:00Z",
        },
        {
            "number": 12,
            "title": "Review canary PR for queue automation",
            "url": "https://github.com/owner/repo/issues/12",
            "body": _queue_card_body(
                status="review",
                owner="claude-tab-2",
                branch="feat/queue-plan",
                next_action="Review PR and merge to canary if checks are green.",
            ),
            "createdAt": "2026-05-14T10:10:00Z",
            "updatedAt": "2026-05-14T10:20:00Z",
        },
        {
            "number": 13,
            "title": "Optimize qwen GPU memory path",
            "url": "https://github.com/owner/repo/issues/13",
            "body": _queue_card_body(
                status="in-progress",
                owner="codex-main",
                last_heartbeat="2026-05-14T09:00Z @ abc123",
                next_action="Measure CPU copies and move feasible path to Metal/CUDA.",
            ),
            "createdAt": "2026-05-14T10:15:00Z",
            "updatedAt": "2026-05-14T10:25:00Z",
        },
        {
            "number": 14,
            "title": "Available queue card should be claimable",
            "url": "https://github.com/owner/repo/issues/14",
            "body": _queue_card_body(
                status="claimed",
                owner="unclaimed",
                next_action="Claim and replace the legacy sentinel owner.",
            ),
            "createdAt": "2026-05-14T10:30:00Z",
            "updatedAt": "2026-05-14T10:35:00Z",
        },
    ]
    issue_list_file = pathlib.Path(tmp) / "fake_issue_list.json"
    issue_list_file.write_text(json.dumps(issue_list), encoding="utf-8")
    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        f'ISSUE_LIST_FILE="{issue_list_file}"\n'
        "if [ \"$1 $2\" = \"issue list\" ]; then\n"
        "  cat \"$ISSUE_LIST_FILE\"\n"
        "  exit 0\n"
        "fi\n"
        "printf '%s\\n' 'unexpected gh call' >&2\n"
        "exit 2\n",
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env


def _isolated_env_with_create_fake_gh(tmp: str) -> tuple[dict[str, str], pathlib.Path]:
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir()
    record_dir = pathlib.Path(tmp) / "gh-record"
    record_dir.mkdir()
    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        f'RECORD_DIR="{record_dir}"\n'
        'for a in "$@"; do printf "%s\\n" "$a"; done > "$RECORD_DIR/create-argv.txt"\n'
        'if [ "$1 $2" = "issue create" ]; then\n'
        '  while [ $# -gt 0 ]; do\n'
        '    case "$1" in\n'
        '      --title) shift; printf "%s\\n" "$1" > "$RECORD_DIR/create-title.txt" ;;\n'
        '      --body-file) shift; cp "$1" "$RECORD_DIR/create-body.txt" ;;\n'
        '    esac\n'
        '    shift || true\n'
        '  done\n'
        "  printf '%s\\n' 'https://github.com/owner/repo/issues/99'\n"
        '  exit 0\n'
        'fi\n'
        "printf '%s\\n' 'unexpected gh call' >&2\n"
        "exit 2\n",
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env, record_dir


def _isolated_env_with_adopt_fake_gh(
    tmp: str, issue: dict[str, object]
) -> tuple[dict[str, str], pathlib.Path]:
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir()
    record_dir = pathlib.Path(tmp) / "gh-record"
    record_dir.mkdir()
    issue_json = record_dir / "issue.json"
    issue_json.write_text(json.dumps(issue), encoding="utf-8")
    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        f'ISSUE_JSON="{issue_json}"\n'
        f'RECORD_DIR="{record_dir}"\n'
        'if [ "$1 $2" = "issue view" ]; then\n'
        '  cat "$ISSUE_JSON"\n'
        '  exit 0\n'
        'fi\n'
        'for a in "$@"; do printf "%s\\n" "$a"; done > "$RECORD_DIR/argv.txt"\n'
        'if [ "$1 $2" = "issue edit" ]; then\n'
        '  saw_body=0\n'
        '  saw_label=0\n'
        '  while [ $# -gt 0 ]; do\n'
        '    case "$1" in\n'
        '      --body-file)\n'
        '        shift\n'
        '        cp "$1" "$RECORD_DIR/edited-body.txt"\n'
        '        saw_body=1\n'
        '        ;;\n'
        '      --add-label)\n'
        '        saw_label=1\n'
        '        ;;\n'
        '    esac\n'
        '    shift || true\n'
        '  done\n'
        '  if [ "$saw_body" = "1" ]; then cp "$RECORD_DIR/argv.txt" "$RECORD_DIR/body-edit-argv.txt"; fi\n'
        '  if [ "$saw_label" = "1" ]; then cp "$RECORD_DIR/argv.txt" "$RECORD_DIR/label-edit-argv.txt"; fi\n'
        "  printf '%s\\n' 'ok'\n"
        '  exit 0\n'
        'fi\n'
        "printf '%s\\n' 'unexpected gh call' >&2\n"
        "exit 2\n",
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env, record_dir


class QueueDispatchTests(unittest.TestCase):
    def test_queue_no_subcommand_defaults_to_plan(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "owner/repo", "--owner", "codex-main"],
                env_overrides=_isolated_env_with_plan_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("# airc queue plan — owner/repo", result.stdout)
        self.assertIn("summary:", result.stdout)
        self.assertIn("## Strategic lanes", result.stdout)
        self.assertIn("alpha-gap/rust-runtime", result.stdout)
        self.assertIn("perf/resource-control", result.stdout)

    def test_queue_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "--help"], env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("PR-1", result.stdout)
        self.assertIn("queue plan", result.stdout)

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

    def test_queue_plan_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "plan", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cohesive prioritized kanban", result.stdout)
        self.assertIn("alpha-gap/rust-runtime", result.stdout)

    def test_queue_adopt_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "adopt", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Original issue body", result.stdout)

    def test_unknown_subcommand_fails_loudly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "frobnicate"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown subcommand", result.stdout + result.stderr)


class QueuePlanTests(unittest.TestCase):
    def test_plan_json_groups_lanes_and_priorities(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "plan", "owner/repo", "--owner", "codex-main", "--json"],
                env_overrides=_isolated_env_with_plan_fake_gh(tmp),
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["repo"], "owner/repo")
        self.assertEqual(payload["summary"]["open"], 4)
        self.assertEqual(payload["summary"]["unowned"], 2)
        self.assertGreaterEqual(payload["summary"]["priorities"]["P0"], 1)
        self.assertIn("alpha-gap/rust-runtime", payload["lanes"])
        self.assertIn("perf/resource-control", payload["lanes"])
        self.assertIn("flywheel/automation", payload["lanes"])
        self.assertIn("owner/repo#11", payload["lanes"]["alpha-gap/rust-runtime"])
        self.assertIn("codex-main", payload["owners"])
        rust_card = next(card for card in payload["cards"] if card["ref"] == "owner/repo#11")
        self.assertIn("airc queue claim 'owner/repo#11'", rust_card["claim_command"])
        sentinel_card = next(card for card in payload["cards"] if card["ref"] == "owner/repo#14")
        self.assertEqual(sentinel_card["owner"], "")
        self.assertEqual(sentinel_card["stale_reason"], "")

    def test_plan_human_includes_action_sections(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "kanban", "owner/repo", "--owner", "codex-main"],
                env_overrides=_isolated_env_with_plan_fake_gh(tmp),
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        self.assertIn("## P0 now", out)
        self.assertIn("## Review / merge candidates", out)
        self.assertIn("## Active ownership", out)
        self.assertIn("## Stale / needs nudge", out)
        self.assertIn("## Next actions", out)
        self.assertIn("Review/merge owner/repo#12", out)


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
        self.assertIn("  title:   test card\n", out)
        self.assertNotIn("airc-queue: test card", out)
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          out, re.DOTALL)
        self.assertIsNotNone(match, f"expected JSON card block; got:\n{out}")
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card.get("kind"), "airc-queue-card-v1")
        self.assertEqual(card.get("owner"), "claude-tab-2")
        self.assertEqual(card.get("branch"), "feat/x")

    def test_create_preserves_title_exactly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env, record_dir = _isolated_env_with_create_fake_gh(tmp)
            result = run_airc(
                ["queue", "add", "owner/repo",
                 "--title", "queue: stop prefixing issue titles",
                 "--owner", "codex-main"],
                env_overrides=env,
            )
            created_title = (record_dir / "create-title.txt").read_text(encoding="utf-8").strip()
            create_argv = (record_dir / "create-argv.txt").read_text(encoding="utf-8").splitlines()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(created_title, "queue: stop prefixing issue titles")
        self.assertIn("--label", create_argv)
        self.assertIn("airc-queue", create_argv)

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
        # No --owner → owner field comes from the first-class session/work
        # identity. Must be non-empty even before `airc join`.
        out = self._dry_run()
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          out, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertIn("owner", card)
        self.assertGreater(len(card["owner"]), 0,
                           "default owner must resolve to SOMETHING")

    def test_dry_run_default_owner_prefers_registered_work_identity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            registered = run_airc(
                ["identity", "register", "--name", "codex-main"],
                env_overrides=env,
            )
            self.assertEqual(registered.returncode, 0, registered.stderr)
            result = run_airc(
                ["queue", "add", "owner/repo",
                 "--title", "test card",
                 "--dry-run"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          result.stdout, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card["owner"], "codex-main")

    def test_whoami_prints_transport_and_work_identity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            registered = run_airc(
                ["identity", "register", "--name", "claude-tab-1"],
                env_overrides=env,
            )
            self.assertEqual(registered.returncode, 0, registered.stderr)
            result = run_airc(["whoami"], env_overrides=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("transport:", result.stdout)
        self.assertIn("work:", result.stdout)
        self.assertIn("claude-tab-1", result.stdout)

    def test_dry_run_default_owner_prefers_queue_env(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            env["AIRC_QUEUE_OWNER"] = "codex-main"
            result = run_airc(
                ["queue", "add", "owner/repo",
                 "--title", "test card",
                 "--dry-run"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          result.stdout, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card["owner"], "codex-main")

    def test_dry_run_default_owner_accepts_agent_name_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            env["AIRC_AGENT_NAME"] = "claude-tab-2"
            result = run_airc(
                ["queue", "add", "owner/repo",
                 "--title", "test card",
                 "--dry-run"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          result.stdout, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card["owner"], "claude-tab-2")

    def test_add_with_owner_unclaimed_omits_owner_field(self) -> None:
        out = self._dry_run("--owner", "unclaimed")
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          out, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertNotIn("owner", card)


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


class QueueAdoptTests(unittest.TestCase):
    def test_adopt_dry_run_prepends_queue_envelope_and_preserves_body(self) -> None:
        issue = {
            "title": "old backlog item",
            "body": "Existing issue text with `markdown` and $(literal).",
        }
        with tempfile.TemporaryDirectory() as tmp:
            env, _record_dir = _isolated_env_with_adopt_fake_gh(tmp, issue)
            result = run_airc(
                ["queue", "adopt", "owner/repo#7",
                 "--owner", "codex",
                 "--env", "triage",
                 "--next-action", "Decide whether this stale issue still applies.",
                 "--dry-run"],
                env_overrides=env,
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DRY RUN", result.stdout)
        self.assertIn("Original issue body", result.stdout)
        self.assertIn("Existing issue text", result.stdout)
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          result.stdout, re.DOTALL)
        self.assertIsNotNone(match)
        card = json.loads(match.group(1))  # type: ignore[union-attr]
        self.assertEqual(card["kind"], "airc-queue-card-v1")
        self.assertEqual(card["id"], "#7")
        self.assertEqual(card["owner"], "codex")
        self.assertEqual(card["status"], "claimed")
        self.assertEqual(card["env"], "triage")

    def test_adopt_posts_body_via_body_file_and_adds_label(self) -> None:
        issue = {"title": "old backlog item", "body": "Original body"}
        with tempfile.TemporaryDirectory() as tmp:
            env, record_dir = _isolated_env_with_adopt_fake_gh(tmp, issue)
            result = run_airc(
                ["queue", "adopt", "owner/repo#7",
                 "--owner", "codex",
                 "--next-action", "Pick this up."],
                env_overrides=env,
            )
            edited_body = (record_dir / "edited-body.txt").read_text(encoding="utf-8")
            body_argv = (record_dir / "body-edit-argv.txt").read_text(encoding="utf-8").splitlines()
            label_argv = (record_dir / "label-edit-argv.txt").read_text(encoding="utf-8").splitlines()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Adopted owner/repo#7", result.stdout)
        self.assertIn("--body-file", body_argv)
        self.assertNotIn("--body", body_argv)
        self.assertIn("--add-label", label_argv)
        self.assertIn("airc-queue-card-v1", edited_body)
        self.assertIn("Original body", edited_body)

    def test_adopt_rejects_existing_queue_card_without_force(self) -> None:
        issue = {
            "title": "already adopted",
            "body": "```json\n{\"kind\":\"airc-queue-card-v1\"}\n```",
        }
        with tempfile.TemporaryDirectory() as tmp:
            env, _record_dir = _isolated_env_with_adopt_fake_gh(tmp, issue)
            result = run_airc(
                ["queue", "adopt", "owner/repo#7", "--dry-run"],
                env_overrides=env,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already has an airc-queue-card-v1 envelope",
                      result.stdout + result.stderr)

    # ─── airc#613 — `--owner unclaimed` sentinel normalization ───────────

    def _adopt_card_dry_run(self, owner_args: list[str]) -> dict:
        """Run adopt --dry-run with the given --owner args and return the
        parsed JSON envelope from the output. Helper for the airc#613
        normalization tests below."""
        issue = {"title": "fresh backlog", "body": "Plain body."}
        with tempfile.TemporaryDirectory() as tmp:
            env, _record_dir = _isolated_env_with_adopt_fake_gh(tmp, issue)
            result = run_airc(
                ["queue", "adopt", "owner/repo#42", "--dry-run", *owner_args],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0,
                         f"adopt dry-run must succeed; stderr={result.stderr}")
        match = re.search(r'```json\s*\n\s*(\{.*?\})\s*\n\s*```',
                          result.stdout, re.DOTALL)
        self.assertIsNotNone(match,
                             f"expected JSON envelope; got:\n{result.stdout}")
        return json.loads(match.group(1))  # type: ignore[union-attr]

    def test_adopt_with_owner_unclaimed_omits_owner_field(self) -> None:
        """`--owner unclaimed` is a sentinel meaning "no owner / available
        for claim" used during bulk adoption. Per airc#613, the sentinel
        must be normalized to absence-of-owner rather than written as the
        literal string "unclaimed" — otherwise the subsequent
        `airc queue claim` fails airc#612 collision protection because
        owner=unclaimed reads as an active owner.

        Catches: regression where the literal "unclaimed" string ends up
        in the envelope's owner field, blocking plain claim."""
        card = self._adopt_card_dry_run(["--owner", "unclaimed"])
        self.assertNotIn(
            "owner", card,
            f"--owner unclaimed should produce no owner field; got: {card}",
        )

    def test_adopt_with_empty_owner_omits_owner_field(self) -> None:
        """`--owner ""` is the explicit "no owner" form. Per airc#613, the
        explicit-empty signal must NOT be silently overwritten with the
        running agent's resolve_name — that's only the default when
        --owner wasn't passed at all.

        Catches: regression where the auto-fill fallback fires even when
        --owner was explicitly set to empty."""
        card = self._adopt_card_dry_run(["--owner", ""])
        self.assertNotIn(
            "owner", card,
            f'--owner "" should produce no owner field; got: {card}',
        )

    def test_adopt_default_owner_falls_back_to_resolve_name(self) -> None:
        """When --owner is NOT passed, owner defaults to the running
        agent's resolve_name (existing behavior, kept intact). This test
        guards the fallback so the airc#613 fix doesn't accidentally
        break the common path of `airc queue adopt <ref>` with no flags.

        Catches: regression where the fallback gets disabled along with
        the sentinel normalization."""
        card = self._adopt_card_dry_run([])
        self.assertIn(
            "owner", card,
            f"adopt with no --owner should auto-fill owner; got: {card}",
        )
        self.assertNotEqual(
            card["owner"], "",
            "auto-filled owner should be a non-empty resolve_name",
        )
        self.assertNotEqual(
            card["owner"], "unclaimed",
            "auto-filled owner should be a real handle, not the sentinel",
        )


if __name__ == "__main__":
    unittest.main()
