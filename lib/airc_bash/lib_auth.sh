# Sourced by airc. Self-healing gh-auth detection + recovery.
#
# Why this exists: gh's keyring token can silently invalidate (token
# revoked / 2FA flow expired / brew upgrade / OS keychain rotation /
# laptop sleep across an OAuth boundary). Joel reports this is FREQUENT
# in practice. Pre-fix, every airc command path that touched the gist
# substrate would die() with a message saying "run gh auth login -h
# github.com" and STOP. The user then had to manually re-auth.
#
# Two problems with that:
#   1. The next user (Carl, Toby, anyone) hits the same wall on first
#      use after a token expires. Manual fix per user = unfixed bug.
#   2. Joel's "no manual fixes" / "script must self-heal" rule is
#      universal: an error a user will hit must be one a SCRIPT
#      surfaces a path through, not a command in a docstring.
#
# Joel: "airc MUST BE THE INSTIGATOR" — the trigger to re-auth must
# come from airc itself when it detects the failure, not from the user
# remembering the command. Joel only does the browser click.
#
# Joel: "if it is actually required" — DON'T trigger preemptively or
# on every command. Specifically distinguish:
#   - real keyring-invalid    → self-heal IS required, trigger flow
#   - secondary rate limit    → token is fine; don't re-auth, just wait
#   - gh not installed        → can't fix from here; report + die
#   - scope missing only      → re-auth with -s gist (we always request gist)
#
# Detection (airc_detect_gh_auth_state) is split from action
# (airc_self_heal_gh_auth) so callers control when re-auth is allowed
# (interactive contexts only).

# ── airc_detect_gh_auth_state — echo one of {ok, invalid, rate_limited, not_installed} ──
#
# Probes gh's auth state without side-effects. Output goes to STDOUT
# as a single line (caller captures with command substitution). Any
# explanatory text goes to STDERR.
#
# State definitions:
#   ok            — gh exists, /user reachable, token valid + has gist scope
#   invalid       — gh exists, but /user returns 401 AND /rate_limit ALSO fails
#                   (the keyring token is genuinely dead — both endpoints would
#                    succeed if it were just secondary-limited)
#   rate_limited  — gh exists, /user 403'd by secondary rate limit, but
#                   /rate_limit still works → token is FINE, just wait
#   not_installed — gh binary not on PATH; can't diagnose further
airc_detect_gh_auth_state() {
  if ! command -v gh >/dev/null 2>&1; then
    echo "not_installed"
    return 0
  fi

  if gh auth status >/dev/null 2>&1; then
    echo "ok"
    return 0
  fi

  # gh auth status failed. Two possibilities:
  # (a) Real auth failure — keyring token is dead.
  # (b) Secondary rate limit — gh's `auth status` probes /user which
  #     gets 403'd, then prints "token invalid" misleadingly. The
  #     /rate_limit endpoint is reachable during secondary rate limits;
  #     if it works, the token is fine. (issue #341 in airc)
  #
  # The KEYRING-INVALID case is what Joel reports as common (and is
  # what just happened on M5: heartbeat.stderr 403'd Apr 30 with the
  # rate-limit-shaped error message but `gh api rate_limit` ALSO failed
  # → token is dead, not rate-limited).
  if gh api rate_limit >/dev/null 2>&1; then
    echo "rate_limited"
  else
    echo "invalid"
  fi
}

# ── airc_self_heal_gh_auth — trigger gh's interactive web login flow ──
#
# Runs `gh auth login --web -s gist` IN-PROCESS. gh prints a one-time
# device code, opens the user's browser to github.com/login/device,
# and waits for the user to paste the code + click "Authorize".
#
# Args:
#   $1 — context string ("airc connect", "airc send foo", etc.) shown
#        to the user so they understand WHY airc is opening a browser
#        unprompted.
#
# Returns:
#   0 — gh auth succeeded; caller should retry whatever failed
#   1 — gh auth failed (user cancelled, no network, no TTY, etc.); caller
#       should fall back to die() with its existing error message
#
# Constraints:
#   - Always requests the `gist` scope (airc's substrate is gist-based;
#     a token without gist scope publishes 403, breaking all channels).
#   - Pins to github.com (only host airc supports).
#   - HTTPS git protocol (avoids ssh-key prompt during the flow).
#   - Refuses to run in non-interactive contexts (background flush
#     loops, daemon heartbeat). Those should queue + emit a clear
#     "auth broken" log line and let the next interactive call self-heal.
#   - Caller is responsible for confirming auth_state == invalid before
#     calling. This function does NOT re-detect — pass-through.
airc_self_heal_gh_auth() {
  local context="${1:-airc operation}"

  if ! command -v gh >/dev/null 2>&1; then
    echo "" >&2
    echo "  ✗ gh CLI not installed — can't self-heal." >&2
    echo "    Install: brew install gh   (or https://cli.github.com)" >&2
    echo "" >&2
    return 1
  fi

  # Refuse non-interactive contexts. Background processes have no human
  # at the keyboard to paste a device code; triggering the flow there
  # would just hang the process forever.
  if [ ! -t 0 ] || [ ! -t 1 ]; then
    echo "  ✗ Auth broken but stdin/stdout not a TTY — can't run interactive re-auth here." >&2
    echo "    Re-run an airc CLI command (airc status / airc connect / airc send …)" >&2
    echo "    in your terminal; it will detect the broken auth + trigger the browser." >&2
    return 1
  fi

  echo "" >&2
  echo "  ⚠  airc detected an invalid GitHub token — triggering re-authentication." >&2
  echo "     Context: $context" >&2
  echo "" >&2
  echo "     gh will print a one-time device code + open your browser." >&2
  echo "     Paste the code in the browser, grant 'gist' scope, then airc resumes." >&2
  echo "" >&2

  # `--web` is the device-code flow. gh prints the code, opens the
  # browser via the OS opener (open / xdg-open / cmd.exe), and blocks
  # until the user completes the flow OR Ctrl-C cancels.
  #
  # `--git-protocol https` skips the ssh/https protocol prompt.
  # `-s gist` requests the gist scope explicitly (default token doesn't
  # carry it; without gist scope, channel publishes get a 403).
  if gh auth login --web --hostname github.com --git-protocol https -s gist; then
    echo "" >&2
    echo "  ✓ gh auth restored — resuming $context." >&2
    echo "" >&2
    return 0
  fi

  echo "" >&2
  echo "  ✗ gh auth flow did not complete (cancelled? no network?)." >&2
  echo "    Re-run your airc command to try again." >&2
  echo "" >&2
  return 1
}

# ── airc_ensure_gh_auth_or_heal — the full detect+react state machine ──
#
# Higher-level wrapper for command surfaces (cmd_connect, cmd_send,
# cmd_status, cmd_doctor, …). Encapsulates the {detect → react} cycle
# so each caller is one line:
#
#   airc_ensure_gh_auth_or_heal "airc join" || die "..."
#
# Behaviour by detected state:
#   ok            → return 0; caller proceeds
#   rate_limited  → emit explanation; return 1 (token is fine, just wait)
#   invalid       → trigger self-heal browser flow; on success re-detect
#                   to confirm + return 0; on failure emit fallback +
#                   return 1 (caller dies with its own message)
#   not_installed → emit install-gh hint; return 1
#
# The auth_state echoed on stderr is the SAME identifier the
# airc_detect_gh_auth_state helper produces, so callers can grep their
# logs for it if they want to react differently per state.
#
# Args:
#   $1 — context string for any messages / self-heal flow
#
# Returns:
#   0 — auth is OK after this call (either was OK, or was healed)
#   1 — auth is NOT OK (rate_limited, invalid + heal failed, not_installed)
airc_ensure_gh_auth_or_heal() {
  local context="${1:-airc operation}"
  local state; state="$(airc_detect_gh_auth_state)"

  case "$state" in
    ok)
      return 0
      ;;
    rate_limited)
      echo "" >&2
      echo "  ! GitHub secondary rate limit (abuse detection) triggered." >&2
      echo "    Your token is fine — wait 5-15 minutes and retry." >&2
      echo "    Context: $context" >&2
      echo "" >&2
      echo "    Why this is confusing: 'gh auth status' calls /user which gets 403'd" >&2
      echo "    during secondary rate limits; gh then prints 'token invalid'. The" >&2
      echo "    /rate_limit endpoint is reachable, which proves the token works." >&2
      echo "" >&2
      echo "    Caused by: too many gh API calls in a short window (polling loops," >&2
      echo "    rapid-fire PR/issue/comment activity, etc.)." >&2
      return 1
      ;;
    invalid)
      if airc_self_heal_gh_auth "$context"; then
        if [ "$(airc_detect_gh_auth_state)" = "ok" ]; then
          return 0
        fi
        echo "  ✗ gh auth flow completed but state still not OK." >&2
        echo "    Possible: scope grant was skipped, or token wrote but read-back lag." >&2
        echo "    Re-run your airc command to retry." >&2
        return 1
      fi
      # Self-heal didn't run or didn't complete (no TTY, gh missing,
      # user cancelled). Emit the manual fallback so users without
      # interactive shells still know what to do.
      echo "" >&2
      echo "  ✗ gh CLI is installed but the GitHub token is invalid." >&2
      echo "    Detail:" >&2
      gh auth status 2>&1 | sed 's/^/      /' >&2
      echo "" >&2
      echo "    Manual fix: gh auth login -h github.com -s gist" >&2
      echo "" >&2
      echo "    Without gh auth, airc can't talk to the gist substrate at all." >&2
      return 1
      ;;
    not_installed)
      echo "" >&2
      echo "  ✗ gh CLI not installed — required for airc's gist substrate." >&2
      echo "    Install: brew install gh   (or https://cli.github.com)" >&2
      echo "    Then:    gh auth login -h github.com -s gist" >&2
      echo "" >&2
      return 1
      ;;
    *)
      echo "  ✗ Unknown gh auth state: '$state' (this is an airc bug)" >&2
      return 1
      ;;
  esac
}
