"""Tests for `_airc_gh_safe_body` — shell-safe gh body writes (airc#571).

Coverage:
  - helper exists in lib_gh.sh and is sourced unconditionally by `airc`
  - helper invokes gh with --body-file (NEVER --body) so backticks /
    fenced code blocks / $-vars in the body can't trigger shell command
    substitution and can't blow argv length limits
  - body bytes round-trip exactly (newlines, leading/trailing whitespace,
    no spurious extra newline appended by the helper)
  - all three current call-site modules (cmd_knock, cmd_approve,
    cmd_queue) route through the helper — greppable invariant
  - error path: gh non-zero exit propagates; temp file is cleaned up
  - tiny edge: helper rejects missing args (return 2, no gh call)

The fake `gh` here is a recording stub: it captures every --body-file
content into a known location so the test can assert what got passed to
gh. Real GitHub auth is never exercised.
"""

from __future__ import annotations

import os
import pathlib
import re
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
AIRC_BIN = REPO_ROOT / "airc"
LIB_GH = REPO_ROOT / "lib" / "airc_bash" / "lib_gh.sh"


def _isolated_env(tmp: str) -> dict[str, str]:
    return {
        "HOME": tmp,
        "AIRC_HOME": str(pathlib.Path(tmp) / ".airc"),
        "AIRC_NO_IDENTITY_PROMPT": "1",
        "PATH": "/usr/bin:/bin",
    }


def _make_recording_gh(tmp: str, exit_code: int = 0,
                      stdout_text: str = "https://github.com/owner/repo/issues/9999\n",
                      stderr_text: str = "") -> tuple[pathlib.Path, pathlib.Path]:
    """Build a fake `gh` script that:
      - copies every --body-file argument's content to record_dir/body.txt
      - records every --body argument text to record_dir/body-arg.txt
        (so the test can assert it's NEVER touched)
      - records the full argv to record_dir/argv.txt
      - records `issue close` argv to record_dir/close-argv.txt
      - exits with `exit_code` after printing stdout/stderr
    Returns (gh_path, record_dir).
    """
    fakebin = pathlib.Path(tmp) / "bin"
    fakebin.mkdir(exist_ok=True)
    record_dir = pathlib.Path(tmp) / "gh-record"
    record_dir.mkdir(exist_ok=True)
    gh = fakebin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        f'RECORD_DIR="{record_dir}"\n'
        f'EXIT_CODE={exit_code}\n'
        # Save full argv (one per line) so the test can grep for --body / --body-file.
        'for a in "$@"; do printf "%s\\n" "$a"; done > "$RECORD_DIR/argv.txt"\n'
        'while [ $# -gt 0 ]; do\n'
        '  case "$1" in\n'
        '    --body-file)\n'
        '      shift\n'
        '      cat "$RECORD_DIR/argv.txt" > "$RECORD_DIR/body-argv.txt"\n'
        '      cp "$1" "$RECORD_DIR/body.txt" 2>/dev/null || true\n'
        '      shift\n'
        '      ;;\n'
        '    --body)\n'
        '      shift\n'
        '      printf "%s" "$1" > "$RECORD_DIR/body-arg.txt"\n'
        '      shift\n'
        '      ;;\n'
        '    *) shift ;;\n'
        '  esac\n'
        'done\n'
        'case "$(cat "$RECORD_DIR/argv.txt" 2>/dev/null | head -2 | tr "\\n" " ")" in\n'
        '  "issue close "*) cat "$RECORD_DIR/argv.txt" > "$RECORD_DIR/close-argv.txt" ;;\n'
        'esac\n'
        f'printf "%s" {repr(stdout_text)}\n'
        + (f'printf "%s" {repr(stderr_text)} >&2\n' if stderr_text else '')
        + 'exit $EXIT_CODE\n',
        encoding="utf-8",
    )
    gh.chmod(0o755)
    return gh, record_dir


def _run_helper_directly(body: str, gh_args: list[str], tmp: str,
                         exit_code: int = 0,
                         stdout_text: str = "https://github.com/owner/repo/issues/9999\n",
                         stderr_text: str = "",
                         extra_env: dict[str, str] | None = None
                         ) -> tuple[subprocess.CompletedProcess[str], pathlib.Path]:
    """Source lib_gh.sh in a sub-bash and call _airc_gh_safe_body with
    the supplied body + gh args. Returns (CompletedProcess, record_dir).

    The recording fake-gh is on PATH so the helper invokes IT instead of
    real gh; the body file content lands in record_dir/body.txt for
    assertion.

    Body is written to a temp file and read back inside the bash script
    via `cat` — that way the body bytes are NEVER re-quoted on the
    bash command line (which is exactly the failure mode we're testing
    the helper protects against; the test must not become an instance
    of the bug it's testing).
    """
    gh, record_dir = _make_recording_gh(tmp, exit_code=exit_code,
                                        stdout_text=stdout_text,
                                        stderr_text=stderr_text)
    env = _isolated_env(tmp)
    env["PATH"] = f"{gh.parent}:/usr/bin:/bin"
    if extra_env:
        env.update(extra_env)

    # Body via temp file, read back with `cat -- "$BODY_FILE"`.
    body_file = pathlib.Path(tmp) / "test-body-input.txt"
    body_file.write_text(body, encoding="utf-8")
    env["BODY_FILE"] = str(body_file)

    # Args via temp script that literally inlines them — Python's
    # shlex.quote is the right tool for this job because it produces
    # POSIX-safe single-quoted argv that bash sees as opaque tokens.
    import shlex
    inline_args = " ".join(shlex.quote(a) for a in gh_args)

    bash_script = (
        f'source "{LIB_GH}"\n'
        # Read body bytes verbatim from the file (no shell interpretation).
        'BODY=$(cat -- "$BODY_FILE")\n'
        # Re-add a trailing newline ONLY if the input file had one — $(cat)
        # strips trailing newlines per POSIX. Probe via wc + tail.
        'if [ "$(tail -c 1 -- "$BODY_FILE" | od -An -c | tr -d " ")" = "\\n" ]; then\n'
        '  BODY="$BODY"$\'\\n\'\n'
        'fi\n'
        f'_airc_gh_safe_body "$BODY" {inline_args}\n'
    )
    result = subprocess.run(
        ["bash", "-c", bash_script],
        capture_output=True, text=True, env=env, timeout=10,
    )
    return result, record_dir


def _run_safe_close_directly(comment: str, tmp: str,
                             exit_code: int = 0,
                             stdout_text: str = "ok\n",
                             stderr_text: str = "",
                             issue_num: str = "9",
                             repo: str = "owner/repo"
                             ) -> tuple[subprocess.CompletedProcess[str], pathlib.Path]:
    """Call _airc_gh_safe_issue_close with a recording fake gh."""
    gh, record_dir = _make_recording_gh(tmp, exit_code=exit_code,
                                        stdout_text=stdout_text,
                                        stderr_text=stderr_text)
    env = _isolated_env(tmp)
    env["PATH"] = f"{gh.parent}:/usr/bin:/bin"
    comment_file = pathlib.Path(tmp) / "close-comment.txt"
    comment_file.write_text(comment, encoding="utf-8")
    env["COMMENT_FILE"] = str(comment_file)
    env["ISSUE_NUM"] = issue_num
    env["REPO"] = repo
    bash_script = (
        f'source "{LIB_GH}"\n'
        'COMMENT=$(cat -- "$COMMENT_FILE")\n'
        'if [ "$(tail -c 1 -- "$COMMENT_FILE" | od -An -c | tr -d " ")" = "\\n" ]; then\n'
        '  COMMENT="$COMMENT"$\'\\n\'\n'
        'fi\n'
        '_airc_gh_safe_issue_close "$ISSUE_NUM" "$REPO" "$COMMENT"\n'
    )
    result = subprocess.run(
        ["bash", "-c", bash_script],
        capture_output=True, text=True, env=env, timeout=10,
    )
    return result, record_dir


class HelperRoundtripTests(unittest.TestCase):
    """Body bytes survive the helper unchanged."""

    def test_plain_body_roundtrips(self) -> None:
        body = "hello world\n"
        with tempfile.TemporaryDirectory() as tmp:
            result, record_dir = _run_helper_directly(
                body, ["issue", "create", "--repo", "owner/repo", "--title", "T"],
                tmp,
            )
            files = list(record_dir.iterdir())
            file_names = [f.name for f in files]
            body_file = record_dir / "body.txt"
            body_text = body_file.read_text(encoding="utf-8") if body_file.exists() else None
        self.assertEqual(result.returncode, 0,
                         f"helper must succeed; stderr={result.stderr!r} stdout={result.stdout!r}")
        self.assertTrue(body_text is not None,
                        f"fake gh must have seen --body-file; "
                        f"record_dir files={file_names}; "
                        f"stdout={result.stdout!r}; stderr={result.stderr!r}")
        self.assertEqual(body_text, body,
                         "body bytes must round-trip exactly")

    def test_backticks_inert_no_command_substitution(self) -> None:
        # If the helper accidentally evaluated the body, `whoami` would
        # be replaced by the username. The helper MUST NOT do that.
        body = "Status: `whoami` should stay literal, not become $(whoami).\n"
        with tempfile.TemporaryDirectory() as tmp:
            result, record_dir = _run_helper_directly(
                body, ["issue", "create", "--repo", "owner/repo", "--title", "T"],
                tmp,
            )
            round_tripped = (record_dir / "body.txt").read_text(encoding="utf-8")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(round_tripped, body)
        self.assertIn("`whoami`", round_tripped,
                      "literal backticks must survive — no command substitution")
        self.assertNotIn(os.environ.get("USER", "should-not-appear-here"),
                         round_tripped.replace("whoami", ""),
                         "username must not have leaked into the body")

    def test_fenced_code_block_with_dollar_vars(self) -> None:
        body = (
            "Card body:\n"
            "\n"
            "```bash\n"
            "echo $HOME\n"
            "echo $(date)\n"
            "echo `pwd`\n"
            "```\n"
            "\n"
            "Done.\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            result, record_dir = _run_helper_directly(
                body, ["issue", "create", "--repo", "owner/repo", "--title", "T"],
                tmp,
            )
            round_tripped = (record_dir / "body.txt").read_text(encoding="utf-8")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(round_tripped, body,
                         "fenced code block with $-vars + $() + backticks "
                         "must survive byte-for-byte")

    def test_no_trailing_newline_added(self) -> None:
        # Body without trailing \n: helper must not silently grow it.
        body = "no trailing newline"
        with tempfile.TemporaryDirectory() as tmp:
            result, record_dir = _run_helper_directly(
                body, ["issue", "create", "--repo", "owner/repo", "--title", "T"],
                tmp,
            )
            round_tripped = (record_dir / "body.txt").read_text(encoding="utf-8")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(round_tripped, body, "no trailing newline appended")


class HelperFlagDisciplineTests(unittest.TestCase):
    """The helper must use --body-file, never --body."""

    def test_argv_uses_body_file_not_body(self) -> None:
        body = "anything"
        with tempfile.TemporaryDirectory() as tmp:
            result, record_dir = _run_helper_directly(
                body, ["issue", "edit", "9", "--repo", "owner/repo"],
                tmp,
            )
            argv = (record_dir / "argv.txt").read_text(encoding="utf-8").splitlines()
            body_arg_seen = (record_dir / "body-arg.txt").exists()
        self.assertEqual(result.returncode, 0)
        self.assertIn("--body-file", argv,
                      "helper must pass --body-file to gh")
        self.assertNotIn("--body", argv,
                         "helper must NEVER pass --body to gh — argv-length "
                         "limit + future heredoc-construction footgun")
        self.assertFalse(body_arg_seen,
                         "fake gh must not have recorded a --body argument")


class HelperErrorPathTests(unittest.TestCase):
    """gh failure propagates; helper rejects bad input."""

    def test_gh_failure_propagates_with_stderr_in_stdout(self) -> None:
        body = "x"
        with tempfile.TemporaryDirectory() as tmp:
            result, _ = _run_helper_directly(
                body, ["issue", "create", "--repo", "owner/repo", "--title", "T"],
                tmp, exit_code=1,
                stdout_text="HTTP 422: validation failed\n",
                stderr_text="gh stderr text\n",
            )
        self.assertEqual(result.returncode, 1,
                         "helper must propagate gh's non-zero exit")
        self.assertIn("HTTP 422", result.stdout,
                      "gh's stdout must reach caller")
        self.assertIn("gh stderr text", result.stdout,
                      "gh's stderr must be folded into stdout (2>&1 contract)")

    def test_missing_body_arg_returns_2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            result = subprocess.run(
                ["bash", "-c",
                 f'source "{LIB_GH}"; _airc_gh_safe_body'],
                capture_output=True, text=True, env=env, timeout=5,
            )
        self.assertEqual(result.returncode, 2,
                         "missing args must return 2, not silently call gh")
        self.assertIn("_airc_gh_safe_body", result.stderr)


class SafeIssueCloseTests(unittest.TestCase):
    """Closeout comments use body-file, then close without inline comment."""

    def test_close_comment_uses_body_file_then_plain_close(self) -> None:
        comment = (
            "Closed after PR #1.\n\n"
            "Literal markdown must survive: `airc queue nudge owner/repo` and $(date).\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            result, record_dir = _run_safe_close_directly(comment, tmp)
            round_tripped = (record_dir / "body.txt").read_text(encoding="utf-8")
            body_argv = (record_dir / "body-argv.txt").read_text(encoding="utf-8").splitlines()
            close_argv = (record_dir / "close-argv.txt").read_text(encoding="utf-8").splitlines()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(round_tripped, comment)
        self.assertIn("--body-file", body_argv)
        self.assertNotIn("--body", body_argv)
        self.assertEqual(close_argv[:3], ["issue", "close", "9"])
        self.assertNotIn("--comment", close_argv,
                         "issue close must not receive inline Markdown comment")

    def test_close_without_comment_does_not_post_comment(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result, record_dir = _run_safe_close_directly("", tmp)
            body_file_seen = (record_dir / "body.txt").exists()
            close_argv = (record_dir / "close-argv.txt").read_text(encoding="utf-8").splitlines()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(body_file_seen, "empty comment must not call issue comment")
        self.assertEqual(close_argv[:3], ["issue", "close", "9"])

    def test_safe_close_missing_args_returns_2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env = _isolated_env(tmp)
            result = subprocess.run(
                ["bash", "-c",
                 f'source "{LIB_GH}"; _airc_gh_safe_issue_close'],
                capture_output=True, text=True, env=env, timeout=5,
            )
        self.assertEqual(result.returncode, 2)
        self.assertIn("_airc_gh_safe_issue_close", result.stderr)


class CallSiteWiringTests(unittest.TestCase):
    """All three current modules route gh body writes through the helper.

    Greppable invariant — if a future PR adds a `gh ... --body "$x"`
    site, this test fails and points the author at the helper.
    """

    MODULES = (
        REPO_ROOT / "lib" / "airc_bash" / "cmd_knock.sh",
        REPO_ROOT / "lib" / "airc_bash" / "cmd_approve.sh",
        REPO_ROOT / "lib" / "airc_bash" / "cmd_queue.sh",
    )

    def test_no_module_uses_raw_body_flag(self) -> None:
        offenders: list[str] = []
        # Match `--body ` (with trailing space, which is how it's used as
        # a flag) but NOT `--body-file`. Also exclude `# --body ...` comments.
        flag_re = re.compile(r'(?<!#)\s--body(?:\s|=)(?!file)')
        for path in self.MODULES:
            text = path.read_text(encoding="utf-8")
            for lineno, line in enumerate(text.splitlines(), start=1):
                stripped = line.lstrip()
                if stripped.startswith("#"):
                    continue
                if flag_re.search(line):
                    offenders.append(f"{path.name}:{lineno}: {line.strip()}")
        self.assertEqual(offenders, [],
                         "raw `--body` flag found — switch to "
                         "_airc_gh_safe_body (lib_gh.sh, airc#571):\n"
                         + "\n".join(offenders))

    def test_helper_sourced_unconditionally_by_airc(self) -> None:
        # _airc_gh_safe_body must be loaded regardless of which subcommand
        # dispatches — it's a core safety net, not an opt-in.
        text = AIRC_BIN.read_text(encoding="utf-8")
        self.assertIn("airc_bash/lib_gh.sh", text,
                      "lib_gh.sh must be sourced by the airc dispatcher")

    def test_each_module_calls_helper_at_least_once(self) -> None:
        # Sanity: every module that posts to gh must actually USE the
        # helper. If a module stops calling helper entirely, that's a
        # regression — likely someone replaced it with raw gh.
        for path in self.MODULES:
            text = path.read_text(encoding="utf-8")
            self.assertIn("_airc_gh_safe_body", text,
                          f"{path.name} must call _airc_gh_safe_body — "
                          "raw gh body writes were removed in airc#571")


if __name__ == "__main__":
    unittest.main()
