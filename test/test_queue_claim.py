"""Tests for `airc queue claim/release/set-status/heartbeat` (airc#562).

Coverage:
  - dispatch: claim/release/set-status/heartbeat reach the right cmd functions + --help
  - validation: malformed URLs / missing args / bad status enum
  - mutate-card python helper: applies --set/--clear correctly to a fixture
  - dry-run: prints the would-be body, doesn't call gh
  - status log: appends a chronological entry on every mutation
  - claim defaults: session/work identity, then compatibility env fallback; status=in-progress
  - claim/heartbeat stamp last_heartbeat
  - release defaults: clears owner, sets status=claimed
  - release --status blocked allowed; in-progress/review/merged rejected

Real `gh issue view` + `gh issue edit` aren't exercised (need real auth).
The dry-run path covers everything UP TO the gh call so the body shape
is verified end-to-end against a synthetic issue body fed via fake gh.
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


def _card_body(owner: str | None = "previous-owner",
               status: str = "claimed") -> str:
    owner_line = f'  "owner": "{owner}",\n' if owner is not None else ""
    return f'''**airc-queue card**

Coordinates work via the AIRC queue substrate (airc#562). Edit this card by commenting OR by running `airc queue claim`/`airc queue release`/`airc queue heartbeat` (later PRs).

```json
{{
  "kind": "airc-queue-card-v1",
{owner_line}  "status": "{status}",
  "branch": "feat/x"
}}
```

Close this issue when the work is done (status=merged/abandoned).
'''


SYNTHETIC_CARD_BODY = _card_body()


def _isolated_env_with_fake_gh(tmp: str, body_response: str | None = None) -> dict[str, str]:
    """Set up a fake `gh` that returns the synthetic card body on `gh issue view`
    so claim/release/set-status can exercise their parse → mutate → write path
    in --dry-run mode without hitting real GitHub."""
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir(exist_ok=True)
    body = body_response if body_response is not None else SYNTHETIC_CARD_BODY
    body_file = pathlib.Path(tmp) / "fake_body.txt"
    body_file.write_text(body, encoding="utf-8")
    gh_script = (
        "#!/bin/sh\n"
        f'BODY_FILE="{body_file}"\n'
        "case \"$1 $2\" in\n"
        "  'issue view') cat \"$BODY_FILE\" ;;\n"
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


class QueueDispatchPR2Tests(unittest.TestCase):
    def test_claim_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "claim", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--owner", result.stdout)
        self.assertIn("--status", result.stdout)

    def test_release_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "release", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--reason", result.stdout)

    def test_set_status_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "set-status", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("claimed", result.stdout)

    def test_heartbeat_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "heartbeat", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--note", result.stdout)
        self.assertIn("last_heartbeat", result.stdout)

    def test_top_help_lists_pr2_verbs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        for verb in ("claim", "release", "set-status", "heartbeat"):
            self.assertIn(verb, result.stdout,
                          f"top-level help must list '{verb}' verb")


class QueueClaimValidationTests(unittest.TestCase):
    def test_missing_url_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "claim"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)

    def test_malformed_url_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "claim", "not-a-url"],
                              env_overrides=_isolated_env_with_fake_gh(tmp))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("owner/repo", result.stdout + result.stderr)

    def test_short_issue_ref_parses_on_macos_bash3(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "claim", "owner/repo#1", "--owner", "codex", "--dry-run"],
                env_overrides=_isolated_env_with_fake_gh(
                    tmp,
                    body_response=_card_body(owner=None),
                ),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DRY RUN", result.stdout)
        self.assertIn("owner/repo#1", result.stdout)
        self.assertNotIn("local: -n", result.stderr)

    def test_bad_status_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "claim", "owner/repo#1", "--status", "in-flight"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        for canonical in ("claimed", "in-progress", "blocked", "review", "merged"):
            self.assertIn(canonical, result.stdout + result.stderr)


class QueueReleaseValidationTests(unittest.TestCase):
    def test_missing_url_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "release"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)

    def test_release_status_must_be_claimable_or_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "release", "owner/repo#1",
                 "--status", "in-progress"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertNotEqual(result.returncode, 0,
                            "release to in-progress must be rejected; "
                            "in-progress implies an active owner")
        combined = result.stdout + result.stderr
        self.assertIn("set-status", combined,
                      "error must point operator at set-status for "
                      "non-release status changes")


class QueueSetStatusValidationTests(unittest.TestCase):
    def test_missing_state_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "set-status", "owner/repo#1"],
                env_overrides=_isolated_env(tmp),
            )
        self.assertNotEqual(result.returncode, 0)

    def test_bad_state_rejected_with_canonical_list(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "set-status", "owner/repo#1", "frobnicated"],
                env_overrides=_isolated_env_with_fake_gh(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        for canonical in ("claimed", "in-progress", "blocked", "review", "merged"):
            self.assertIn(canonical, result.stdout + result.stderr)


class QueueMutateBodyShapeTests(unittest.TestCase):
    """Dry-run end-to-end: parse synthetic body, apply mutation, verify
    output body has the new envelope + status-log entry."""

    def _dry_run_extract_body(self, args: list[str], body: str | None = None) -> str:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp, body_response=body)
            result = run_airc(args + ["--dry-run"], env_overrides=env)
        self.assertEqual(result.returncode, 0,
                         f"dry-run must succeed; stderr={result.stderr}")
        # Body lines are indented "    " by the printf in _airc_queue_mutate_card.
        lines = result.stdout.splitlines()
        body_start = None
        for i, line in enumerate(lines):
            if line.strip() == "new body:":
                body_start = i + 1
                break
        self.assertIsNotNone(body_start,
                             f"dry-run output missing 'new body:' marker;\n{result.stdout}")
        body_lines = []
        for line in lines[body_start:]:
            if line.startswith("    "):
                body_lines.append(line[4:])
            elif line == "":
                body_lines.append("")
            else:
                break
        return "\n".join(body_lines)

    def _extract_envelope(self, body: str) -> dict:
        match = re.search(r'```json\s*\n(.*?)\n\s*```', body, re.DOTALL)
        self.assertIsNotNone(match, f"no JSON envelope in body:\n{body}")
        return json.loads(match.group(1).strip())  # type: ignore[union-attr]

    def test_claim_sets_owner_and_status(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "claim", "owner/repo#1", "--owner", "claude-tab-2", "--force"]
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["owner"], "claude-tab-2")
        self.assertEqual(envelope["status"], "in-progress")
        self.assertRegex(envelope["last_heartbeat"], r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z")
        # Other pre-existing fields preserved.
        self.assertEqual(envelope["branch"], "feat/x")
        # Status log appended.
        self.assertIn("## Status log", body)
        self.assertIn("claim by claude-tab-2", body)

    def test_claim_default_owner_prefers_queue_env(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp, body_response=_card_body(owner=None))
            env["AIRC_QUEUE_OWNER"] = "codex-main"
            result = run_airc(
                ["queue", "claim", "owner/repo#1", "--dry-run"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"owner": "codex-main"', result.stdout)
        self.assertIn("claim by codex-main", result.stdout)

    def test_claim_default_owner_prefers_registered_work_identity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp, body_response=_card_body(owner=None))
            registered = run_airc(
                ["identity", "register", "--name", "banach"],
                env_overrides=env,
            )
            self.assertEqual(registered.returncode, 0, registered.stderr)
            result = run_airc(
                ["queue", "claim", "owner/repo#1", "--dry-run"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"owner": "banach"', result.stdout)
        self.assertIn("claim by banach", result.stdout)

    def test_claim_rejects_different_active_owner_by_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(
                tmp,
                body_response=_card_body(owner="claude-tab-1", status="in-progress"),
            )
            result = run_airc(
                ["queue", "claim", "owner/repo#1", "--owner", "claude-tab-2", "--dry-run"],
                env_overrides=env,
            )
        self.assertNotEqual(result.returncode, 0)
        combined = result.stdout + result.stderr
        self.assertIn("already claimed by 'claude-tab-1'", combined)
        self.assertIn("Use --force", combined)
        self.assertNotIn("DRY RUN", result.stdout)

    def test_claim_force_allows_handoff_from_different_owner(self) -> None:
        body = self._dry_run_extract_body(
            [
                "queue", "claim", "owner/repo#1",
                "--owner", "claude-tab-2",
                "--force",
            ],
            body=_card_body(owner="claude-tab-1", status="in-progress"),
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["owner"], "claude-tab-2")
        self.assertEqual(envelope["status"], "in-progress")

    def test_claim_allows_same_owner_without_force(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "claim", "owner/repo#1", "--owner", "claude-tab-1"],
            body=_card_body(owner="claude-tab-1", status="in-progress"),
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["owner"], "claude-tab-1")
        self.assertEqual(envelope["status"], "in-progress")

    def test_claim_allows_unclaimed_card_without_force(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "claim", "owner/repo#1", "--owner", "claude-tab-2"],
            body=_card_body(owner=None, status="claimed"),
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["owner"], "claude-tab-2")
        self.assertEqual(envelope["status"], "in-progress")

    def test_claim_allows_merged_card_without_force(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "claim", "owner/repo#1", "--owner", "claude-tab-2"],
            body=_card_body(owner="claude-tab-1", status="merged"),
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["owner"], "claude-tab-2")
        self.assertEqual(envelope["status"], "in-progress")

    def test_heartbeat_sets_owner_and_last_heartbeat(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "heartbeat", "owner/repo#1",
             "--owner", "codex",
             "--note", "still testing"]
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["owner"], "codex")
        self.assertEqual(envelope["status"], "claimed",
                         "heartbeat without --status must preserve status")
        self.assertRegex(envelope["last_heartbeat"], r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z")
        self.assertIn("heartbeat by codex", body)
        self.assertIn("still testing", body)

    def test_heartbeat_default_owner_prefers_agent_name(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env_with_fake_gh(tmp)
            env["AIRC_AGENT_NAME"] = "claude-tab-2"
            result = run_airc(
                ["queue", "heartbeat", "owner/repo#1", "--dry-run"],
                env_overrides=env,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"owner": "claude-tab-2"', result.stdout)
        self.assertIn("heartbeat by claude-tab-2", result.stdout)

    def test_heartbeat_can_update_status(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "heartbeat", "owner/repo#1",
             "--owner", "codex",
             "--status", "in-progress"]
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["owner"], "codex")
        self.assertEqual(envelope["status"], "in-progress")

    def test_release_clears_owner_and_reverts_status(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "release", "owner/repo#1",
             "--reason", "yielding to sibling"]
        )
        envelope = self._extract_envelope(body)
        self.assertNotIn("owner", envelope,
                         "release must REMOVE the owner field, not blank it")
        self.assertEqual(envelope["status"], "claimed",
                         "release default revert status is 'claimed'")
        self.assertIn("yielding to sibling", body,
                      "reason must appear in the status log")

    def test_release_to_blocked_allowed(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "release", "owner/repo#1",
             "--status", "blocked",
             "--reason", "waiting on image push"]
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["status"], "blocked")
        self.assertNotIn("owner", envelope)

    def test_set_status_changes_only_status(self) -> None:
        body = self._dry_run_extract_body(
            ["queue", "set-status", "owner/repo#1", "review"]
        )
        envelope = self._extract_envelope(body)
        self.assertEqual(envelope["status"], "review")
        # Owner field preserved unchanged.
        self.assertEqual(envelope["owner"], "previous-owner")
        self.assertIn("status=review", body)

    def test_status_log_section_created_when_absent(self) -> None:
        # Body without an existing ## Status log section: mutation must
        # CREATE the section + append the first entry.
        body_in = SYNTHETIC_CARD_BODY  # no Status log header
        self.assertNotIn("## Status log", body_in,
                         "fixture sanity: input body has no log section")
        body_out = self._dry_run_extract_body(
            ["queue", "set-status", "owner/repo#1", "review"],
            body=body_in,
        )
        self.assertIn("## Status log", body_out,
                      "mutation must create Status log section if absent")
        # And the entry exists.
        self.assertIn("status=review", body_out)


if __name__ == "__main__":
    unittest.main()
