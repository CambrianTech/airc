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

import json
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

SECOND_CARD_BODY = '''**airc-queue card**

Second sample card.

```json
{
  "kind": "airc-queue-card-v1",
  "owner": "codex",
  "status": "review",
  "branch": "feat/repo-nudge"
}
```
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
    issue = {
        "title": "Sample card for nudge tests",
        "body": body,
    }
    issue_blob = json.dumps(issue)
    issue_file = pathlib.Path(tmp) / "fake_issue.json"
    issue_file.write_text(issue_blob, encoding="utf-8")
    issue_list = [
        {
            "number": 1,
            "title": "airc-queue: Sample card for nudge tests",
            "url": "https://github.com/owner/repo/issues/1",
            "body": body,
            "updatedAt": "2026-05-13T21:00:00Z",
        },
        {
            "number": 2,
            "title": "airc-queue: Repo nudge followup",
            "url": "https://github.com/owner/repo/issues/2",
            "body": SECOND_CARD_BODY,
            "updatedAt": "2026-05-13T21:01:00Z",
        },
    ]
    issue_list_file = pathlib.Path(tmp) / "fake_issue_list.json"
    issue_list_file.write_text(json.dumps(issue_list), encoding="utf-8")
    gh_script = (
        "#!/bin/sh\n"
        f'ISSUE_FILE="{issue_file}"\n'
        f'ISSUE_LIST_FILE="{issue_list_file}"\n'
        f'BODY_FILE="{body_file}"\n'
        "case \"$1 $2\" in\n"
        "  'issue list') cat \"$ISSUE_LIST_FILE\" ;;\n"
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
    def test_top_help_lists_next_verb(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("queue next", result.stdout)

    def test_next_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "next", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("recommend next claimable work", result.stdout)
        self.assertIn("--idle-ping", result.stdout)

    def test_metronome_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "metronome", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("automatic queue-next idle pulses", result.stdout)
        self.assertIn("metronome off", result.stdout)

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
        self.assertIn("airc queue pongs", result.stdout)
        self.assertIn("airc queue availability", result.stdout)


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


class QueueRepoNudgeDryRunTests(unittest.TestCase):
    def test_metronome_writes_actionable_monitor_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            result = run_airc(
                ["queue", "metronome", "owner/repo",
                 "--interval", "60",
                 "--owner", "codex-main",
                 "--limit", "7",
                 "--repo-root", "/work/repo"],
                env_overrides=env,
            )
            config = pathlib.Path(env["AIRC_HOME"]) / "queue_metronome"
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Queue metronome every 60s", result.stdout)
            text = config.read_text(encoding="utf-8")
            self.assertIn("repo=owner/repo", text)
            self.assertIn("interval=60", text)
            self.assertIn("owner=codex-main", text)
            self.assertIn("limit=7", text)
            self.assertIn("repo_root=/work/repo", text)

    def test_metronome_rejects_spammy_interval(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "metronome", "owner/repo", "--interval", "5"],
                env_overrides=_isolated_env(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--interval must be >= 30", result.stdout + result.stderr)

    def test_metronome_off_removes_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            result = run_airc(
                ["queue", "metronome", "owner/repo", "--interval", "60"],
                env_overrides=env,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            result = run_airc(["queue", "metronome", "off"], env_overrides=env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((pathlib.Path(env["AIRC_HOME"]) / "queue_metronome").exists())

    def test_next_recommends_claimable_work_with_exact_commands(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "next", "owner/repo",
                 "--owner", "codex-main",
                 "--repo-root", "/work/repo"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        self.assertIn("# airc-queue next — owner/repo", out)
        self.assertIn("owner: codex-main", out)
        self.assertIn("owner/repo#2", out)
        self.assertIn("status: review owner=codex", out)
        self.assertIn("airc queue claim 'owner/repo#2' --owner 'codex-main'", out)
        self.assertIn("airc lane create 'owner/repo#2' --base 'canary' --branch 'feat/repo-nudge' --repo '/work/repo'", out)

    def test_next_json_shape(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "pick", "owner/repo",
                 "--owner", "codex-main",
                 "--json"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["repo"], "owner/repo")
        self.assertEqual(payload["owner"], "codex-main")
        self.assertEqual(payload["candidates"][0]["ref"], "owner/repo#2")
        self.assertIn("claim_command", payload["candidates"][0])
        self.assertIn("lane_command", payload["candidates"][0])

    def test_repo_scoped_nudge_sends_status_sweep(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        self.assertIn("repo-nudge:", out)
        self.assertIn("owner/repo", out)
        self.assertIn("#1 in-progress owner=claude-tab-1", out)
        self.assertIn("#2 review owner=codex", out)
        self.assertIn("pong with:", out)
        self.assertIn("card=<owner/repo#N|idle>", out)
        self.assertIn("claim=<keep|release|none>", out)

    def test_repo_scoped_nudge_accepts_explicit_sweep_id(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo",
                 "--sweep-id", "sweep-123",
                 "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("sweep=sweep-123", result.stdout)
        self.assertIn("pong: owner/repo — sweep=sweep-123", result.stdout)

    def test_repo_scoped_nudge_with_message_appends_text(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo",
                 "--message", "Bueller status sweep",
                 "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Bueller status sweep", result.stdout)

    def test_repo_scoped_nudge_limit_must_be_integer(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo", "--limit", "many", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--limit", result.stdout + result.stderr)

    def test_repo_scoped_nudge_does_not_require_queue_card_envelope_on_target(self) -> None:
        # owner/repo is a repo scope, not a malformed issue URL. It should
        # list cards and broadcast a sweep, not look for an envelope on
        # a non-existent "repo issue".
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "nudge", "owner/repo", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(tmp, body_response=NON_CARD_BODY),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("repo-nudge:", result.stdout)


class QueuePongsTests(unittest.TestCase):
    def test_pongs_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "pongs", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("summarize repo-nudge replies", result.stdout)
        self.assertIn("--sweep-id", result.stdout)

    def test_pongs_summarizes_responders_and_missing_owners(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp)
            airc_home = pathlib.Path(env["AIRC_HOME"])
            airc_home.mkdir(parents=True, exist_ok=True)
            messages = airc_home / "messages.jsonl"
            messages.write_text(
                "\n".join([
                    json.dumps({
                        "ts": "2099-01-01T00:00:00Z",
                        "from": "airc-8a5e",
                        "msg": (
                            "repo-nudge: owner/repo — sweep=sweep-123 — "
                            "pong with: pong: owner/repo — sweep=sweep-123 — "
                            "<nick> — card=<owner/repo#N|idle> "
                            "state=<idle|coding|testing|reviewing|blocked>"
                        ),
                    }),
                    json.dumps({
                        "ts": "2099-01-01T00:00:01Z",
                        "from": "claude-tab-1",
                        "msg": (
                            "pong: owner/repo — sweep=sweep-123 — claude-tab-1 — "
                            "card=<owner/repo#1> state=<coding> blocker=<none> "
                            "next=<finish tests> claim=<keep>"
                        ),
                    }),
                    json.dumps({
                        "ts": "2099-01-01T00:00:02Z",
                        "from": "someone-else",
                        "msg": "unrelated message",
                    }),
                ]) + "\n",
                encoding="utf-8",
            )

            result = run_airc(
                ["queue", "pongs", "owner/repo",
                 "--sweep-id", "sweep-123",
                 "--since", "2000-01-01T00:00:00Z"],
                env_overrides=env,
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("responders (1)", result.stdout)
        self.assertIn("claude-tab-1", result.stdout)
        self.assertNotIn("repo-nudge:", result.stdout)
        self.assertIn("card=owner/repo#1", result.stdout)
        self.assertIn("state=coding", result.stdout)
        self.assertIn("missing owners (1): codex", result.stdout)

    def test_pongs_json_filters_by_sweep_id(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp)
            airc_home = pathlib.Path(env["AIRC_HOME"])
            airc_home.mkdir(parents=True, exist_ok=True)
            (airc_home / "messages.jsonl").write_text(
                "\n".join([
                    json.dumps({
                        "ts": "2099-01-01T00:00:00Z",
                        "from": "airc-8a5e",
                        "msg": (
                            "repo-nudge: owner/repo — sweep=sweep-next — "
                            "pong with: pong: owner/repo — sweep=sweep-next — "
                            "<nick> — card=<owner/repo#N|idle> "
                            "state=<idle|coding|testing|reviewing|blocked>"
                        ),
                    }),
                    json.dumps({
                        "ts": "2099-01-01T00:00:01Z",
                        "from": "claude-tab-1",
                        "msg": "pong: owner/repo — sweep=old — claude-tab-1 — card=<owner/repo#1> state=<coding> blocker=<none> next=<x> claim=<keep>",
                    }),
                    json.dumps({
                        "ts": "2099-01-01T00:00:02Z",
                        "from": "codex",
                        "msg": "pong: owner/repo — sweep=new — codex — card=<owner/repo#2> state=<reviewing> blocker=<none> next=<merge> claim=<keep>",
                    }),
                ]) + "\n",
                encoding="utf-8",
            )

            result = run_airc(
                ["queue", "pongs", "owner/repo",
                 "--sweep-id", "new",
                 "--since", "2000-01-01T00:00:00Z",
                 "--json"],
                env_overrides=env,
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["sweep_id"], "new")
        self.assertEqual([r["nick"] for r in payload["responders"]], ["codex"])
        self.assertEqual(payload["missing_owners"], ["claude-tab-1"])


class QueueAvailabilityTests(unittest.TestCase):
    def test_availability_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "availability", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("stale-claim", result.stdout)
        self.assertIn("--stale-after", result.stdout)

    def test_availability_summarizes_activity_and_stale_claims(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp)
            airc_home = pathlib.Path(env["AIRC_HOME"])
            airc_home.mkdir(parents=True, exist_ok=True)
            (airc_home / "messages.jsonl").write_text(
                "\n".join([
                    json.dumps({
                        "ts": "2099-01-01T00:00:01Z",
                        "from": "claude-tab-1",
                        "msg": "working on owner/repo#1",
                    }),
                    json.dumps({
                        "ts": "2099-01-01T00:00:02Z",
                        "from": "codex",
                        "msg": (
                            "pong: owner/repo — sweep=sweep-123 — codex — "
                            "card=<owner/repo#2> state=<reviewing> "
                            "blocker=<none> next=<merge> claim=<keep>"
                        ),
                    }),
                ]) + "\n",
                encoding="utf-8",
            )

            result = run_airc(
                ["queue", "availability", "owner/repo",
                 "--since", "2000-01-01T00:00:00Z",
                 "--sweep-id", "sweep-next"],
                env_overrides=env,
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        out = result.stdout
        self.assertIn("# airc-queue availability — owner/repo", out)
        self.assertIn("repo-nudge responders (1)", out)
        self.assertIn("codex: card=owner/repo#2 state=reviewing", out)
        self.assertNotIn("airc-8a5e: card=owner/repo#N|idle", out)
        self.assertIn("recent room activity (2)", out)
        self.assertIn("claude-tab-1", out)
        self.assertIn("attention needed (2)", out)
        self.assertIn("reason=missing-heartbeat", out)
        self.assertIn("missing owners: none", out)
        self.assertIn("airc queue nudge owner/repo --sweep-id sweep-next", out)
        self.assertIn("airc queue pongs owner/repo --sweep-id sweep-next", out)

    def test_availability_json_shape(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp)
            airc_home = pathlib.Path(env["AIRC_HOME"])
            airc_home.mkdir(parents=True, exist_ok=True)
            (airc_home / "messages.jsonl").write_text(
                json.dumps({
                    "ts": "2099-01-01T00:00:02Z",
                    "from": "codex",
                    "msg": "pong: owner/repo — sweep=s1 — codex — card=<owner/repo#2> state=<testing> blocker=<none> next=<ship> claim=<keep>",
                }) + "\n",
                encoding="utf-8",
            )

            result = run_airc(
                ["queue", "avail", "owner/repo",
                 "--since", "2000-01-01T00:00:00Z",
                 "--json"],
                env_overrides=env,
            )

        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        self.assertEqual(payload["repo"], "owner/repo")
        self.assertEqual(len(payload["cards"]), 2)
        self.assertEqual(len(payload["stale_cards"]), 2)
        self.assertEqual(payload["responders"][0]["nick"], "codex")
        self.assertIn("claude-tab-1", payload["missing_owners"])
        self.assertIn("airc queue nudge owner/repo", payload["suggested_nudge"])


if __name__ == "__main__":
    unittest.main()
