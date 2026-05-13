# Sourced by airc. lib_gh — shared helpers for invoking the `gh` CLI safely
# from airc command modules.
#
# Why this exists (airc#571):
#   GitHub issue/PR/comment bodies are Markdown — they routinely contain
#   backticks, fenced code blocks, $-vars, and single quotes. Two failure
#   modes have bitten Codex on canary:
#
#     1. Heredoc-construction at variable-build time. If a body is
#        constructed via $(cat <<EOF ... EOF) where the EOF terminator
#        isn't single-quoted, embedded backticks/$(...) execute as
#        command substitution AT VARIABLE-BUILD TIME. The resulting body
#        is missing the substituted text or contains command output
#        instead of the literal Markdown.
#
#     2. argv-length limit on `--body "$x"`. ARG_MAX on macOS/Linux is
#        ~256KB. A queue card with a long status log + multiple cards
#        worth of cross-references can approach that. argv overflow is
#        silent: gh runs but with truncated body.
#
#   The fix here is API-shaped: every gh invocation that takes a body
#   goes through `_airc_gh_safe_body`, which writes the body to a temp
#   file and passes it via `--body-file`. That eliminates BOTH classes
#   in one place — callers can't accidentally use `--body` and miss the
#   protection (because the helper hides the flag entirely).
#
# Companion guidance for callers:
#   - Build the body string with single-quoted heredoc terminators
#     (`<<'EOF' ... EOF`) so backticks are inert at construction time.
#   - Then pass that string as the FIRST arg to _airc_gh_safe_body.
#     The helper handles the file-and-flag plumbing.
#
# Functions exported:
#   _airc_gh_safe_body — write a body to a temp file and call gh with
#                        --body-file. See doc-comment on the function.
# ----------------------------------------------------------------------

_airc_gh_safe_body() {
  # Invoke `gh` with a long/Markdown body via --body-file, never --body.
  #
  # USAGE
  #   _airc_gh_safe_body <body-string> <gh-args...>
  #
  # EXAMPLES
  #   _airc_gh_safe_body "$issue_body" issue create \
  #     --repo "$target_repo" --title "$title" --label "airc-knock"
  #
  #   _airc_gh_safe_body "$comment_body" issue comment "$issue_num" \
  #     --repo "$repo"
  #
  #   _airc_gh_safe_body "$new_body" issue edit "$issue_num" \
  #     --repo "$repo"
  #
  # CONTRACT (matches the previous `gh ... 2>&1` pattern callers used)
  #   - Stdout: gh's stdout (URL on success, error text on failure).
  #   - Stderr: also captured to stdout via 2>&1, mirroring the existing
  #     idiom so callers can keep `if out=$(_airc_gh_safe_body ...); then`
  #     unchanged.
  #   - Return code: 0 on gh success, gh's non-zero exit on failure,
  #     2 if the temp file couldn't be created (extremely rare).
  #   - Temp file is removed on every code path including gh failure.
  #
  # WHY a temp file (not stdin or process substitution)
  #   - `--body-file -` would read stdin, but airc's call sites already
  #     compose other commands via $(...) capture; mixing pipe-stdin
  #     with capture-stdout is exactly the contention pattern Codex
  #     caught in cmd_approve.sh / cmd_queue.sh during PR review.
  #   - Process substitution (`<(printf '%s' "$body")`) requires bash
  #     features that vary across the airc-supported shell zoo; the
  #     temp-file path works on bash 3.2 (macOS default) and up.
  if [ "$#" -lt 2 ]; then
    printf 'airc: _airc_gh_safe_body needs <body> <gh-args...>\n' >&2
    return 2
  fi

  local body="$1"
  shift

  local body_file
  if ! body_file=$(mktemp -t airc-gh-body.XXXXXX 2>/dev/null); then
    # Some BusyBox mktemp variants don't accept -t; fall back to TMPDIR.
    local tmpdir="${TMPDIR:-/tmp}"
    body_file="$tmpdir/airc-gh-body.$$.$RANDOM"
    if ! : > "$body_file" 2>/dev/null; then
      printf 'airc: _airc_gh_safe_body could not create temp file under %s\n' "$tmpdir" >&2
      return 2
    fi
  fi

  # printf '%s' is intentional: no trailing newline is appended that the
  # caller didn't already include. Bodies built via heredoc already end
  # with a newline; bodies built via printf '%s\n' do too. Adding a
  # second newline here would silently grow every card on every mutate.
  printf '%s' "$body" > "$body_file"

  local out
  local rc=0
  out=$(gh "$@" --body-file "$body_file" 2>&1) || rc=$?

  rm -f "$body_file"

  # Preserve gh's output verbatim. No trailing newline added — same
  # contract as $(gh ... 2>&1) which the call sites previously used.
  printf '%s' "$out"
  return $rc
}
