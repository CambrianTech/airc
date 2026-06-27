#!/usr/bin/env bash
#
# bootstrap-airc.sh -- cold install + first-time setup + room join in one command
#
# Usage:
#   ./bootstrap-airc.sh [mnemonic-or-gist-id]
#   curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/canary/bootstrap-airc.sh \
#     | bash -s -- [mnemonic-or-gist-id]
#
# What it does:
#   1. Runs install.sh if airc isn't already on PATH (handles prereqs +
#      puts ~/.airc/src/airc on PATH).
#   2. Runs `airc doctor --connect` to verify the env can pair (catches
#      Tailscale-down / gh-missing / network-out before they silently fail).
#   3. Walks gh auth if not already done.
#   4. Joins a room: with the mnemonic-or-gist-id argument if given,
#      otherwise auto-scope from the current git repo (or #general).
#   5. Sets a default identity if pronouns are still unset.
#   6. Prints a final whois + next-step hints.
#
# Designed for first-time users (especially first-EXTERNAL users like
# Toby) so the path from "got the SMS with a 4-word phrase" to "in the
# room" is a single command, not seven.
#
# Issue #81. Pairs with bootstrap-airc.ps1 for Windows native.

set -euo pipefail

MNEMONIC="${1:-}"

step() { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
warn() { printf '  \033[1;33m!\033[0m %s\n' "$*" >&2; }
die()  { printf '\n\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# 1. install if not present
if ! command -v airc >/dev/null 2>&1; then
  step "airc not on PATH -- running installer (canary channel)"
  curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/canary/install.sh | bash
  # Pick up the freshly-installed binary in this same session.
  export PATH="$HOME/.airc/src:$PATH"
  if ! command -v airc >/dev/null 2>&1; then
    die "airc still not on PATH after install. Add ~/.airc/src to PATH and re-run."
  fi
  ok "airc installed: $(command -v airc)"
else
  ok "airc already on PATH: $(command -v airc)"
fi

# 2. pre-flight (live route/process state — catches daemon/route issues
#    before join). The rust-rewrite `airc doctor` exposes `--health` for
#    this; the old `--connect` flag no longer exists and made this
#    pre-flight hard-fail on every fresh rust install. Fixed 2026-06-13.
step "Pre-flight: airc doctor --health"
if ! airc doctor --health; then
  die "Pre-flight failed. Fix the items above, then re-run this script."
fi

# 3. gh auth if needed. Match install.sh's invocation exactly:
#    -h github.com pins the host (avoids the interactive host picker),
#    -s gist requests the scope the substrate needs. After a successful
#    login, wire gh's token into git's credential helper so gist
#    fetch/push (airc's rendezvous hot path) doesn't pop a password
#    prompt on every op. Without setup-git, auth-after-install left the
#    helper unwired — caught live on Windows 2026-06-13.
if ! gh auth status >/dev/null 2>&1; then
  step "Authenticating gh (need 'gist' scope for room substrate)"
  gh auth login -h github.com -s gist
fi
# Idempotent (no-op if already configured); safe to always run.
if ! git config --global --get-all credential.https://github.com.helper 2>/dev/null | grep -q 'gh auth git-credential'; then
  gh auth setup-git 2>/dev/null && ok "gh token wired into git credential helper" || true
fi

# 3b. Git author identity. install.sh sets this too, but on the bootstrap
# path install.sh runs BEFORE gh auth (non-TTY curl|bash), so its
# identity block is skipped — and without this the first agent commit
# dies with "Author identity unknown". Derive from the authenticated gh
# account when unset; never clobber an identity the user already set.
# Public email, else the <id>+<login> noreply alias. No hardcoded values.
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  _need_name=0; _need_email=0
  if [ -z "$(git config --global user.name 2>/dev/null || true)" ]; then _need_name=1; fi
  if [ -z "$(git config --global user.email 2>/dev/null || true)" ]; then _need_email=1; fi
  if [ "$_need_name" = 1 ] || [ "$_need_email" = 1 ]; then
    _gh_login="$(gh api user --jq '.login' 2>/dev/null || true)"
    _gh_name="$(gh api user --jq '.name // .login' 2>/dev/null || true)"
    _gh_id="$(gh api user --jq '.id' 2>/dev/null || true)"
    _gh_email="$(gh api user --jq '.email // empty' 2>/dev/null || true)"
    if [ -z "$_gh_email" ] && [ -n "$_gh_id" ] && [ -n "$_gh_login" ]; then
      _gh_email="${_gh_id}+${_gh_login}@users.noreply.github.com"
    fi
    if [ "$_need_name" = 1 ] && [ -n "$_gh_name" ]; then
      git config --global user.name "$_gh_name"
      ok "git user.name set from gh: $_gh_name"
    fi
    if [ "$_need_email" = 1 ] && [ -n "$_gh_email" ]; then
      git config --global user.email "$_gh_email"
      ok "git user.email set from gh: $_gh_email"
    fi
  fi
fi

# 4. join the room
if [ -n "$MNEMONIC" ]; then
  step "Joining room via mnemonic / gist-id: $MNEMONIC"
  airc join "$MNEMONIC"
else
  step "Joining auto-scoped room (no mnemonic given -- using git remote org or #general)"
  airc join
fi

# Give the pair handshake a moment to settle before identity check.
sleep 1

# 5. set default identity if unset
if airc identity show 2>/dev/null | grep -qE 'pronouns: *\(unset\)'; then
  step "Setting default identity (override later with: airc identity set ...)"
  airc identity set \
    --pronouns it \
    --role onboarded-via-bootstrap \
    --bio "Joined via bootstrap-airc.sh"
fi

# 6. final summary
echo ""
ok "Bootstrap complete. Your airc identity:"
echo ""
airc whois 2>&1 | sed 's/^/    /'
echo ""
ok "Next steps:"
cat <<'EOF'
    airc msg "hello room"           # broadcast to your room
    airc msg @<peer> "hi"           # DM a peer
    airc peers                      # list paired peers
    airc whois <peer>               # see another peer's identity
    airc list                       # see all rooms on your gh account
    airc help                       # full command list
EOF
echo ""
