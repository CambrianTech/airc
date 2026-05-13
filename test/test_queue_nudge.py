"""Tests for `airc queue nudge` (airc#562 PR-3).

Coverage:
  - dispatch: nudge reaches _cmd_queue_nudge + --help
  - top-level help lists nudge
  - validation: missing URL / malformed URL / non-card body fails
  - dry-run prints expected broadcast text + status-log entry, no gh call
  - dry-run with --peer @h emits DM-style text + per-peer log entry
  - dry-run with --message "..." appends the message
  - --peer strips leading @ correctly
  - empty --peer (just "@") fails fast
  - title + status pulled from card envelope into broadcast
  - nudge does NOT mutate status (no --set/--clear in mutate-card call)
  - non-airc-card body rejected before any send

Real `cmd_send` + `gh issue edit` aren't exercised — dry-run path covers
the full envelope-parse + broadcast-text-shape + log-entry-shape end-to-end
without hitting transport.
"""

from __future__ import annotations

import os
import pathlib
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


VALID_CARD_BODY = '''**airc-queue card**

Sample card for nudge tests.

```json
{
  "kind": "airc-queue-card-v1",
  "owner": "claude-tab-1",
  "status": "in-progress",
  "branch": "feat/nudge-test"
}
```

Close this issue when the work is done.
'''

NON_CARD_BODY = '''Plain issue body with no airc-queue-card-v1 envelope.

Just markdown, no JSON code block matching the kind we want.
'''


def _isolated_env_with_fake_gh(tmp: str, body_response: str | None = None) -> dict[str, str]:
    """Stub `gh` so issue-view returns a chosen body and issue-edit no-ops.
    Same shape as test_queue_claim's helper — non-dry-run path would call
    gh to fetch + edit the body, dry-run still calls gh issue view to
    verify the card exists before declining to send/mutate."""
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir(exist_ok=True)
    body = body_response if body_response is not None else VALID_CARD_BODY
    body_file = pathlib.Path(tmp) / "fake_body.txt"
    body_file.write_text(body, encoding="utf-8")
    # Wrap in a JSON envelope so `gh issue view --json title,body` returns
    # a parseable shape — _cmd_queue_nudge consumes title+body together.
    import json
    issue_blob = json.dumps({
        "title": "Sample card for nudge tests",
        "body": body,
    })
    issue_file = pathlib.Path(tmp) / "fake_issue.json"
    issue_file.write_text(issue_blob, encoding="utf-8")
    gh_script = (
        "#!/bin/sh\n"
        f'ISSUE_FILE="{issue_file}"\n'
        f'BODY_FILE="{body_file}"\n'
        "case \"$1 $2\" in\n"
        "  'issue view')\n"
        # If --json present (nudge calls), return the issue blob; else body.
        "    case \" $* \" in\n"
        "      *' --json '*) cat \"$ISSUE_FILE\" ;;\n"
        "      *) cat \"$BODY_FILE\" ;;\n"
        "    esac\n"
        "    ;;\n"
        "  'issue edit') exit 0 ;;\n"
        "  *) echo '[]' ;;\n"
        "esac\n"
    )
    gh = fakebin / "gh"
    gh.write_text(gh_script, encoding="utf-8")
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env


class QueueNudgeDispatchTests(unittest.TestCase):
    def test_nudge_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "nudge", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--peer", result.stdout)
        self.assertIn("--message", result.stdout)
        self.assertIn("--dry-run", result.stdout)

    def test_top_help_lists_nudge_verb(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("nudge", result.stdout,
                      "top-level help must list 'nudge' verb")

    def test_global_help_lists_nudge_verb(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["help"], env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("airc queue nudge", result.stdout)


class QueueNudgeValidationTests(unittest.TestCase):
    def test_missing_url_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "nudge"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)

    def test_malformed_url_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "nudge", "not-a-url"],
                              env_overrides=_isolated_env_with_fake_gh(tmp))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("owner/repo", result.stdout + result.stderr)

    def test_non_card_body_rejected_before_send(self) -> None:
        # nudge must verify the issue is a real airc-queue card BEFORE
        # broadcasting — otherwise random issues could spam the room.
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp, body_response=NON_CARD_BODY),
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("airc-queue-card-v1", result.stdout + result.stderr)

    def test_empty_peer_after_at_strip_fails(self) -> None:
        # --peer "@" alone (just the @ with no handle) must fail fast.
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1", "--peer", "@", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--peer", result.stdout + result.stderr)


class QueueNudgeDryRunTests(unittest.TestCase):
    def test_dry_run_broadcast_includes_card_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        # Broadcast text contains card title + status + claim hint
        self.assertIn("nudge:", out)
        self.assertIn("owner/repo#1", out)
        self.assertIn("Sample card for nudge tests", out)
        self.assertIn("status=in-progress", out)
        self.assertIn("airc queue claim", out)

    def test_dry_run_includes_owner_when_present(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        # Card has owner=claude-tab-1; broadcast surfaces it
        self.assertIn("owner=claude-tab-1", result.stdout)

    def test_dry_run_with_peer_renders_dm_style(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1",
                 "--peer", "@codex", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        # @ strip happened; DM target appears in broadcast text
        self.assertIn("→ @codex", out)
        # Status log line names the target peer
        self.assertIn("nudged @codex", out)

    def test_dry_run_with_peer_no_at_prefix_works(self) -> None:
        # Operators can pass "codex" or "@codex" — both should work.
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1",
                 "--peer", "codex", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        # Same DM rendering whether @ was supplied or not
        self.assertIn("→ @codex", result.stdout)

    def test_dry_run_without_peer_renders_broadcast_style(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        # No "→ @" arrow when no --peer specified
        self.assertNotIn("→ @", out)
        # Status log marks it as broadcast
        self.assertIn("nudged (broadcast)", out)

    def test_dry_run_with_message_appends_text(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1",
                 "--message", "needs eyes before EOD",
                 "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        # Message appears in broadcast text
        self.assertIn("needs eyes before EOD", out)

    def test_dry_run_does_not_mutate_card_status(self) -> None:
        # nudge MUST NOT change the status field (status mutation goes
        # through set-status, not nudge).
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        # The dry-run preview names the log entry but should NOT contain
        # any "set status=" or "would set: status=" affordance.
        # (Card status is RENDERED in the broadcast text — that's the
        # current value being communicated, not a mutation.)
        # Sanity: our log_msg doesn't include "set:" tokens.
        self.assertNotIn("would set: status", out)
        self.assertNotIn("set:status=", out)

    def test_short_issue_ref_parses_on_macos_bash3(self) -> None:
        # Same regression check claude tab #2's PR-2 had — reuses the
        # _airc_queue_parse_issue_url helper which had the bash 3 local -n bug.
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo#1", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("owner/repo#1", result.stdout)
        self.assertNotIn("local: -n", result.stderr)


if __name__ == "__main__":
    unittest.main()
