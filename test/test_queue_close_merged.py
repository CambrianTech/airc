"""Tests for `airc queue close-merged` (airc#576).

Coverage:
  - dispatch: close-merged subcommand reaches _cmd_queue_close_merged + --help works
  - validation: missing PR url, malformed url, --merge-sha sanity
  - PR-not-merged guard: refuses to close cards from an unmerged PR
  - ref parsing: same-repo and cross-repo refs with explicit close keywords
  - envelope verification: skips non-airc-queue issues silently
  - idempotency: closes open cards already at status=merged
  - cross-repo: detected + reported, NOT closed (workflow token scope)
  - dry-run: emits plan without mutating or closing
  - actor flag: shows up in status-log line
  - top-level help advertises close-merged

Real `gh` is never exercised — fake gh wrapper records argv + serves
canned PR/issue JSON from fixture files.
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
        cwd=cwd or str(REPO_ROOT), timeout=20,
    )


def _isolated_env(tmp: str) -> dict[str, str]:
    return {
        "HOME": tmp,
        "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
        "AIRC_NO_IDENTITY_PROMPT": "1",
        "PATH": "/usr/bin:/bin",
    }


def _fake_gh(tmp: str,
             pr_title: str = "",
             pr_body: str = "",
             pr_merged_at: str = "2026-05-13T20:00:00Z",
             pr_base_ref: str = "canary",
             pr_merge_sha: str = "168c666abcdef0123456789",
             pr_url: str = "https://github.com/CambrianTech/airc/pull/574",
             issue_bodies: dict[str, str] | None = None,
             ) -> dict[str, str]:
    """Build a fake gh that:
      - 'gh pr view N --repo X --json ...' returns the canned PR JSON.
      - 'gh issue view N --repo X --json body ...' returns the canned
        issue body for that issue number (or empty body if not in fixtures).
      - 'gh issue edit N --repo X --body-file F' records the body to
        record_dir/edit-<N>.txt and exits 0.
      - 'gh issue close N --repo X ...' records to record_dir/close-<N>.txt.
    Returns the env dict ready for run_airc.
    """
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir(exist_ok=True)
    record_dir = pathlib.Path(tmp) / "gh-record"
    record_dir.mkdir(exist_ok=True)

    # PR JSON file the fake gh streams on `gh pr view`.
    pr_json = {
        "title": pr_title,
        "body": pr_body,
        "mergedAt": pr_merged_at,
        "mergeCommit": {"oid": pr_merge_sha} if pr_merge_sha else None,
        "baseRefName": pr_base_ref,
        "url": pr_url,
    }
    pr_file = pathlib.Path(tmp) / "pr.json"
    pr_file.write_text(json.dumps(pr_json), encoding="utf-8")

    # Per-issue body files. Key = "owner/repo#N" or "N" (for repo-less callers).
    issues_dir = pathlib.Path(tmp) / "issues"
    issues_dir.mkdir(exist_ok=True)
    if issue_bodies:
        for key, body in issue_bodies.items():
            num = key.rsplit("#", 1)[-1] if "#" in key else key
            (issues_dir / f"{num}.txt").write_text(body, encoding="utf-8")

    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        f'RECORD_DIR="{record_dir}"\n'
        f'PR_FILE="{pr_file}"\n'
        f'ISSUES_DIR="{issues_dir}"\n'
        # Save full argv per-call (multiple calls per test possible).
        'CALL_ID=$$_$(date +%s%N 2>/dev/null || echo $$)\n'
        'for a in "$@"; do printf "%s\\n" "$a"; done >> "$RECORD_DIR/argv-$CALL_ID.txt"\n'
        'verb1="$1"\n'
        'verb2="$2"\n'
        'shift 2\n'
        'case "$verb1 $verb2" in\n'
        '  "pr view")\n'
        '    cat "$PR_FILE"\n'
        '    exit 0\n'
        '    ;;\n'
        '  "issue view")\n'
        # First positional arg is the issue number; rest are flags.
        # Real gh supports --jq .body to unwrap. Honor that here so the
        # caller gets the raw body string, matching production behavior.
        '    num="$1"\n'
        '    shift\n'
        '    use_jq=0\n'
        '    while [ $# -gt 0 ]; do\n'
        '      case "$1" in\n'
        '        --jq) use_jq=1; shift; shift ;;\n'
        '        *) shift ;;\n'
        '      esac\n'
        '    done\n'
        '    body_file="$ISSUES_DIR/$num.txt"\n'
        '    if [ -f "$body_file" ]; then\n'
        '      if [ "$use_jq" -eq 1 ]; then\n'
        '        cat "$body_file"\n'
        '      else\n'
        '        printf \'{"body":\'\n'
        '        python3 -c "import json,sys; print(json.dumps(open(sys.argv[1]).read()))" "$body_file"\n'
        '        printf \'}\'\n'
        '      fi\n'
        '    else\n'
        '      if [ "$use_jq" -eq 1 ]; then\n'
        '        :\n'
        '      else\n'
        '        printf \'{"body":""}\'\n'
        '      fi\n'
        '    fi\n'
        '    exit 0\n'
        '    ;;\n'
        '  "issue edit")\n'
        '    num="$1"\n'
        '    while [ $# -gt 0 ]; do\n'
        '      case "$1" in\n'
        '        --body-file) shift; cp "$1" "$RECORD_DIR/edit-$num.txt" 2>/dev/null; shift ;;\n'
        '        *) shift ;;\n'
        '      esac\n'
        '    done\n'
        '    exit 0\n'
        '    ;;\n'
        '  "issue close")\n'
        '    num="$1"\n'
        '    printf "closed\\n" > "$RECORD_DIR/close-$num.txt"\n'
        '    exit 0\n'
        '    ;;\n'
        '  *)\n'
        '    printf "[]"\n'
        '    exit 0\n'
        '    ;;\n'
        'esac\n',
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_env(tmp)
    env["PATH"] = f"{fakebin}:/usr/bin:/bin"
    env["AIRC_GH_BIN"] = str(gh)
    return env


def _card_body(status: str = "in-progress",
               owner: str = "claude-tab-2",
               extra: str = "") -> str:
    """Build a synthetic airc-queue-card-v1 body. Used as the issue body
    a fake gh will return on `gh issue view`."""
    return f'''**airc-queue card**

```json
{{
  "kind": "airc-queue-card-v1",
  "id": "test-card",
  "branch": "feat/test",
  "owner": "{owner}",
  "status": "{status}"
}}
```

{extra}
'''


# ─────────────────────────────────────────────────────────────────
# Dispatch + validation
# ─────────────────────────────────────────────────────────────────

class CloseMergedDispatchTests(unittest.TestCase):
    def test_help_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "close-merged", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--merge-sha", result.stdout)
        self.assertIn("--actor", result.stdout)
        self.assertIn("--dry-run", result.stdout)

    def test_top_level_help_advertises_close_merged(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "--help"],
                              env_overrides=_isolated_env(tmp))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("close-merged", result.stdout,
                      "top-level queue help must list close-merged verb")
        self.assertIn("airc#576", result.stdout)

    def test_unknown_subcommand_error_lists_close_merged(self) -> None:
        # Sanity: when someone typos the verb, the error names close-merged
        # in the available list so they can find it.
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "frobnicate"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("close-merged", result.stdout + result.stderr)


class CloseMergedValidationTests(unittest.TestCase):
    def test_missing_pr_url_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(["queue", "close-merged"],
                              env_overrides=_isolated_env(tmp))
        self.assertNotEqual(result.returncode, 0)

    def test_malformed_pr_url_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _fake_gh(tmp)  # gh exists but won't be reached
            result = run_airc(["queue", "close-merged", "not-a-url"],
                              env_overrides=env)
        self.assertNotEqual(result.returncode, 0)
        combined = result.stdout + result.stderr
        self.assertIn("owner/repo", combined,
                      "error must hint at the right url shape")

    def test_unknown_flag_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run_airc(
                ["queue", "close-merged",
                 "https://github.com/X/Y/pull/1", "--frobnicate"],
                env_overrides=_isolated_env(tmp),
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown flag", result.stdout + result.stderr)


# ─────────────────────────────────────────────────────────────────
# Merged-PR guard
# ─────────────────────────────────────────────────────────────────

class CloseMergedPRGuardTests(unittest.TestCase):
    def test_unmerged_pr_refused(self) -> None:
        # mergedAt empty == PR not merged; refuse to close anything.
        with tempfile.TemporaryDirectory() as tmp:
            env = _fake_gh(tmp,
                           pr_body="Closes #100\n",
                           pr_merged_at="")  # not merged
            result = run_airc(
                ["queue", "close-merged",
                 "https://github.com/CambrianTech/airc/pull/574"],
                env_overrides=env,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not merged", result.stdout + result.stderr)

    def test_no_merge_sha_anywhere_refused(self) -> None:
        # Both --merge-sha unset AND PR metadata mergeCommit absent →
        # refuse, since the status-log entry would have no audit anchor.
        with tempfile.TemporaryDirectory() as tmp:
            env = _fake_gh(tmp,
                           pr_body="Closes #100\n",
                           pr_merge_sha="")  # absent
            result = run_airc(
                ["queue", "close-merged",
                 "https://github.com/CambrianTech/airc/pull/574"],
                env_overrides=env,
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("merge SHA", result.stdout + result.stderr)


# ─────────────────────────────────────────────────────────────────
# Body-ref parser
# ─────────────────────────────────────────────────────────────────

class CloseMergedRefParserTests(unittest.TestCase):
    """Verifies the dry-run output names every queue ref the parser
    extracts. Dry-run prints one [...] line per ref (closed/skip/etc),
    so we count the lines per kind."""

    def _run_dry(self, body: str, env_extras: dict[str, str] | None = None,
                 issue_bodies: dict[str, str] | None = None
                 ) -> tuple[subprocess.CompletedProcess[str], pathlib.Path]:
        tmp = tempfile.mkdtemp()
        env = _fake_gh(tmp, pr_body=body, issue_bodies=issue_bodies or {})
        if env_extras:
            env.update(env_extras)
        result = run_airc(
            ["queue", "close-merged",
             "https://github.com/CambrianTech/airc/pull/574",
             "--dry-run"],
            env_overrides=env,
        )
        return result, pathlib.Path(tmp)

    def test_title_closing_ref_detected(self) -> None:
        tmp = tempfile.mkdtemp()
        env = _fake_gh(
            tmp,
            pr_title="fix: close queue card. Closes #576",
            pr_body="Body has unrelated #561.\n",
            issue_bodies={
                "576": _card_body(),
                "561": "Plain issue, not a queue card.\n",
            },
        )
        result = run_airc(
            ["queue", "close-merged",
             "https://github.com/CambrianTech/airc/pull/581",
             "--dry-run"],
            env_overrides=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("title/body closing refs", result.stdout)
        self.assertIn("CambrianTech/airc#576", result.stdout)
        self.assertIn("[dry-run]", result.stdout)

    def test_title_context_ref_is_not_close_target(self) -> None:
        tmp = tempfile.mkdtemp()
        env = _fake_gh(
            tmp,
            pr_title="feat(#576): document queue cards",
            pr_body="Body has unrelated #561.\n",
            issue_bodies={
                "576": _card_body(),
                "561": "Plain issue, not a queue card.\n",
            },
        )
        result = run_airc(
            ["queue", "close-merged",
             "https://github.com/CambrianTech/airc/pull/581",
             "--dry-run"],
            env_overrides=env,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("nothing to close", result.stdout)
        self.assertNotIn("CambrianTech/airc#576", result.stdout)

    def test_no_refs_clean_exit(self) -> None:
        body = "Just a PR body with no issue refs.\n"
        result, _ = self._run_dry(body)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("nothing to close", result.stdout)

    def test_same_repo_closes_keyword(self) -> None:
        # Closes #100 → should detect as a same-repo ref.
        body = "Body. Closes #100.\n"
        # Issue 100 is a real airc-queue card.
        issue_bodies = {"100": _card_body()}
        result, _ = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("CambrianTech/airc#100", result.stdout)
        self.assertIn("[dry-run]", result.stdout,
                      "dry-run path should mark as would-close")

    def test_bare_hash_n_is_not_a_close_target(self) -> None:
        # #100 without a closing keyword is context only. It must not close
        # implementation cards from docs-only PRs that say "Refs #N".
        body = "See #100 for context.\n"
        issue_bodies = {"100": _card_body()}
        result, _ = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0)
        self.assertIn("nothing to close", result.stdout)
        self.assertNotIn("CambrianTech/airc#100", result.stdout)

    def test_refs_keyword_is_not_a_close_target(self) -> None:
        body = "Refs #100.\n"
        issue_bodies = {"100": _card_body()}
        result, _ = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0)
        self.assertIn("nothing to close", result.stdout)
        self.assertNotIn("CambrianTech/airc#100", result.stdout)

    def test_fixes_keyword_closes_same_repo_ref(self) -> None:
        body = "Fixes #100.\n"
        issue_bodies = {"100": _card_body()}
        result, _ = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0)
        self.assertIn("CambrianTech/airc#100", result.stdout)
        self.assertIn("[dry-run]", result.stdout)

    def test_closing_word_prose_does_not_close_later_ref(self) -> None:
        body = "Fix the queue docs. See #100 for implementation.\n"
        issue_bodies = {"100": _card_body()}
        result, _ = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0)
        self.assertIn("nothing to close", result.stdout)
        self.assertNotIn("CambrianTech/airc#100", result.stdout)

    def test_closing_keyword_accepts_comma_continuation(self) -> None:
        body = "Closes #100, #101 and #102.\n"
        issue_bodies = {"100": _card_body(), "101": _card_body(), "102": _card_body()}
        result, _ = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0)
        self.assertIn("CambrianTech/airc#100", result.stdout)
        self.assertIn("CambrianTech/airc#101", result.stdout)
        self.assertIn("CambrianTech/airc#102", result.stdout)

    def test_cross_repo_ref_detected_not_closed(self) -> None:
        # Cross-repo ref must surface in summary as cross-repo, not closed
        # by default. (--allow-cross-repo flag opt-in is tested separately.)
        body = "Closes CambrianTech/continuum#1130.\n"
        result, _ = self._run_dry(body)
        self.assertEqual(result.returncode, 0)
        self.assertIn("CambrianTech/continuum#1130", result.stdout)
        self.assertIn("[cross-repo]", result.stdout,
                      "cross-repo refs must be marked, not closed")
        self.assertIn("--allow-cross-repo not set", result.stdout,
                      "skip message must explain how to enable cross-repo close")

    def test_dedup_repeated_ref(self) -> None:
        # Same #N referenced twice → process once.
        body = "Closes #100. Also see #100 above.\n"
        issue_bodies = {"100": _card_body()}
        result, _ = self._run_dry(body, issue_bodies=issue_bodies)
        # Count [dry-run] occurrences for #100 — must be exactly 1.
        lines_for_100 = [l for l in result.stdout.splitlines()
                          if "CambrianTech/airc#100" in l and "[dry-run]" in l]
        self.assertEqual(len(lines_for_100), 1,
                         f"expected dedup to keep #100 to ONE line; got:\n{result.stdout}")

    def test_no_false_positive_on_arbitrary_text(self) -> None:
        # Words containing # mid-string should not trigger SAME_RE
        # (the regex requires word-boundary before #). Issue#100 word.
        body = "Just text with no# refs and a Issue#100xyz fragment.\n"
        result, _ = self._run_dry(body)
        self.assertEqual(result.returncode, 0)
        self.assertIn("nothing to close", result.stdout,
                      f"bare text must not match #100; got:\n{result.stdout}")


# ─────────────────────────────────────────────────────────────────
# Envelope-verify + idempotency
# ─────────────────────────────────────────────────────────────────

class CloseMergedEnvelopeTests(unittest.TestCase):
    def _run_dry(self, body: str, issue_bodies: dict[str, str] | None = None
                 ) -> subprocess.CompletedProcess[str]:
        tmp = tempfile.mkdtemp()
        env = _fake_gh(tmp, pr_body=body, issue_bodies=issue_bodies or {})
        return run_airc(
            ["queue", "close-merged",
             "https://github.com/CambrianTech/airc/pull/574",
             "--dry-run"],
            env_overrides=env,
        )

    def test_skips_non_card_silently(self) -> None:
        # Issue 100 has a body but NO airc-queue envelope → skip.
        body = "Closes #100.\n"
        issue_bodies = {"100": "Random issue body, not a queue card.\n"}
        result = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0)
        self.assertIn("[skip]", result.stdout)
        self.assertIn("not an airc-queue card", result.stdout)
        # And the summary should reflect 0 closed.
        self.assertIn("0 closed", result.stdout)

    def test_dry_run_closes_already_merged_card(self) -> None:
        # Idempotent status mutation is not the same as issue closure:
        # if the issue is still open, close-merged must still plan a close.
        body = "Closes #100.\n"
        issue_bodies = {"100": _card_body(status="merged")}
        result = self._run_dry(body, issue_bodies=issue_bodies)
        self.assertEqual(result.returncode, 0)
        self.assertIn("would close already status=merged card", result.stdout)
        self.assertIn("[dry-run]", result.stdout)
        self.assertIn("1 closed", result.stdout)


# ─────────────────────────────────────────────────────────────────
# End-to-end: real mutate + close path through fake gh
# ─────────────────────────────────────────────────────────────────

class CloseMergedE2ETests(unittest.TestCase):
    """Non-dry-run path: verify the fake gh recorded both the issue-edit
    body AND the issue-close call, with the status-log entry reflecting
    the merge SHA + actor."""

    def test_real_close_writes_edit_and_close_for_card(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _fake_gh(
                tmp,
                pr_body="Closes #100.\n",
                pr_merge_sha="168c666abcdef0123456789",
                issue_bodies={"100": _card_body(status="in-progress")},
            )
            result = run_airc(
                ["queue", "close-merged",
                 "https://github.com/CambrianTech/airc/pull/574",
                 "--actor", "github-actions[airc#576]"],
                env_overrides=env,
            )
            record_dir = pathlib.Path(tmp) / "gh-record"
            edit_file = record_dir / "edit-100.txt"
            close_file = record_dir / "close-100.txt"
            edit_body = edit_file.read_text(encoding="utf-8") if edit_file.exists() else None
            close_recorded = close_file.exists()

        self.assertEqual(result.returncode, 0,
                         f"expected success; stdout={result.stdout} stderr={result.stderr}")
        self.assertIn("[closed]", result.stdout)
        self.assertIn("1 closed", result.stdout)

        self.assertIsNotNone(edit_body,
                             "fake gh must have received an edit call for #100")
        self.assertIn('"status": "merged"', edit_body,
                      "edit body must show status=merged")
        # Status-log entry references the PR + sha + actor.
        self.assertIn("Status log", edit_body)
        self.assertIn("168c666a", edit_body,
                      "status-log entry must include the merge SHA prefix")
        self.assertIn("github-actions[airc#576]", edit_body,
                      "actor flag must propagate to the status-log entry")
        self.assertIn("https://github.com/CambrianTech/airc/pull/574", edit_body,
                      "status-log entry must include the PR URL for audit")

        self.assertTrue(close_recorded,
                        "fake gh must have received an issue close call for #100")

    def test_real_close_closes_already_merged_open_card(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _fake_gh(
                tmp,
                pr_body="Closes #100.\n",
                pr_merge_sha="168c666abcdef0123456789",
                issue_bodies={"100": _card_body(status="merged")},
            )
            result = run_airc(
                ["queue", "close-merged",
                 "https://github.com/CambrianTech/airc/pull/574",
                 "--actor", "github-actions[airc#576]"],
                env_overrides=env,
            )
            record_dir = pathlib.Path(tmp) / "gh-record"
            edit_file = record_dir / "edit-100.txt"
            close_file = record_dir / "close-100.txt"
            edit_body = edit_file.read_text(encoding="utf-8") if edit_file.exists() else None
            close_recorded = close_file.exists()

        self.assertEqual(result.returncode, 0,
                         f"expected success; stdout={result.stdout} stderr={result.stderr}")
        self.assertIn("[closed]", result.stdout)
        self.assertIn("already status=merged, issue closed", result.stdout)
        self.assertIn("1 closed", result.stdout)

        self.assertIsNotNone(edit_body,
                             "already-merged close must still append audit log")
        self.assertIn('"status": "merged"', edit_body,
                      "status remains merged")
        self.assertIn("Status log", edit_body)
        self.assertIn("168c666a", edit_body,
                      "audit log must include merge SHA prefix")
        self.assertTrue(close_recorded,
                        "fake gh must have received an issue close call for #100")

    def test_no_close_in_dry_run(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _fake_gh(
                tmp,
                pr_body="Closes #100.\n",
                issue_bodies={"100": _card_body(status="in-progress")},
            )
            result = run_airc(
                ["queue", "close-merged",
                 "https://github.com/CambrianTech/airc/pull/574",
                 "--dry-run"],
                env_overrides=env,
            )
            record_dir = pathlib.Path(tmp) / "gh-record"
            edit_file = record_dir / "edit-100.txt"
            close_file = record_dir / "close-100.txt"

        self.assertEqual(result.returncode, 0)
        self.assertFalse(edit_file.exists(),
                         "dry-run MUST NOT call gh issue edit")
        self.assertFalse(close_file.exists(),
                         "dry-run MUST NOT call gh issue close")


# ─────────────────────────────────────────────────────────────────
# Cross-repo close (--allow-cross-repo flag, continuum#1174)
# ─────────────────────────────────────────────────────────────────

class CloseMergedCrossRepoTests(unittest.TestCase):
    """The --allow-cross-repo flag opts in to attempting close calls
    against issues in OTHER repos. Without the flag (default), cross-
    repo refs are detected + reported but not closed. With the flag,
    the close call is attempted and gh's auth context decides whether
    it actually succeeds.

    These tests use --dry-run so no real gh close happens; they verify
    the dispatch + reporting path differs based on the flag."""

    def _run(self, body: str, extra_args: list[str]
             ) -> subprocess.CompletedProcess[str]:
        tmp = tempfile.mkdtemp()
        env = _fake_gh(tmp, pr_body=body)
        return run_airc(
            ["queue", "close-merged",
             "https://github.com/CambrianTech/airc/pull/574"] + extra_args,
            env_overrides=env,
        )

    def test_default_skips_cross_repo_with_recovery_hint(self) -> None:
        """Default (no flag): cross-repo refs are skipped, message names
        the flag operators need to enable cross-repo close. Backward-
        compat with existing repo-scoped workflows."""
        body = "Closes CambrianTech/continuum#1130.\n"
        result = self._run(body, ["--dry-run"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("[cross-repo]", result.stdout)
        self.assertIn("CambrianTech/continuum#1130", result.stdout)
        self.assertIn("--allow-cross-repo not set", result.stdout,
                      "skip message must explain how to enable cross-repo")
        self.assertIn("1 cross-repo", result.stdout,
                      "summary must count the cross-repo skip")

    def test_allow_cross_repo_attempts_close(self) -> None:
        """With --allow-cross-repo + --dry-run: cross-repo ref reaches
        the dry-run [dry-run] line instead of the [cross-repo] skip
        line. Proves the flag changes the dispatch path."""
        body = "Closes CambrianTech/continuum#1130.\n"
        result = self._run(body, ["--allow-cross-repo", "--dry-run"])
        self.assertEqual(result.returncode, 0,
                         f"dry-run with --allow-cross-repo must succeed; "
                         f"stderr={result.stderr}")
        # The [cross-repo] line still appears (announcing the attempt),
        # but the ref ALSO reaches the dry-run path (would-close summary).
        self.assertIn("[cross-repo]", result.stdout,
                      "cross-repo refs are still announced for visibility")
        self.assertIn("attempting close", result.stdout,
                      "with --allow-cross-repo, message must say attempting")
        self.assertIn("1 cross-repo", result.stdout,
                      "summary still counts the cross-repo ref")

    def test_unknown_flag_still_rejected(self) -> None:
        """Smoke: --allow-cross-repo doesn't disable the unknown-flag
        guard. A typo of the new flag is rejected loudly."""
        body = "Closes #100.\n"
        result = self._run(body, ["--allow-x-repo", "--dry-run"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown flag", result.stdout + result.stderr)


# ─────────────────────────────────────────────────────────────────
# CallSiteWiring sanity (catch a future regression where someone
# adds `--body` raw inside cmd_queue.sh's new code)
# ─────────────────────────────────────────────────────────────────

class CloseMergedHelperWiringTests(unittest.TestCase):
    """The implementation must not introduce raw `--body` calls — the
    airc#571 helper enforced that for every gh body write. This is the
    same greppable invariant test_gh_safe_body.py runs across the
    other modules; we extend it to confirm the new code stays clean."""

    def test_no_raw_body_flag_in_close_merged_path(self) -> None:
        cmd_queue = REPO_ROOT / "lib" / "airc_bash" / "cmd_queue.sh"
        text = cmd_queue.read_text(encoding="utf-8")
        # The mutate path uses _airc_gh_safe_body; close call uses
        # `gh issue close --reason completed` (no body). Anything else
        # touching gh issue/pr in the new code that takes a body MUST
        # go through the helper. If a future PR adds raw `--body` here,
        # this fails.
        import re
        flag_re = re.compile(r'(?<!#)\s--body(?:\s|=)(?!file)')
        offenders = []
        for lineno, line in enumerate(text.splitlines(), start=1):
            stripped = line.lstrip()
            if stripped.startswith("#"):
                continue
            if flag_re.search(line):
                offenders.append(f"{cmd_queue.name}:{lineno}: {line.strip()}")
        self.assertEqual(offenders, [],
                         "raw `--body` flag found in cmd_queue.sh — "
                         "use _airc_gh_safe_body (lib_gh.sh, airc#571):\n"
                         + "\n".join(offenders))


if __name__ == "__main__":
    unittest.main()
